package com.modelmistress.core.tools

import java.io.File
import java.io.IOException
import java.io.InputStream

/**
 * The folder the file tools may read. Every path is resolved inside [root]: absolute paths, `..` and symbolic links that
 * leave the folder are refused. Files with secrets in their names, extensions or folders are never read or listed.
 *
 * [links] are other sandboxes this one may reach, by name. They appear under the folder [LINKS_FOLDER], so `links/notes/a.txt`
 * is `a.txt` in the sandbox called `notes`. Nothing else outside [root] is reachable.
 */
class Workspace(root: File, links: Map<String, File> = emptyMap()) {
    val root: File = root.canonicalFile

    /** The linked folders, by the name they show under [LINKS_FOLDER]. */
    val links: Map<String, File>

    init {
        links.keys.forEach { name ->
            require(name.isNotEmpty() && name == safeName(name) && name != "." && name != "..") {
                "a linked folder needs a plain name"
            }
        }
        this.links = links.mapValues { it.value.canonicalFile }
    }

    /** The file or folder a path names, or an error when the path leaves the workspace. An empty path is the root. */
    fun resolve(relative: String): File {
        val trimmed = relative.trim()
        if (trimmed.startsWith("/") || trimmed.startsWith("\\") || (trimmed.length >= 2 && trimmed[1] == ':')) {
            throw IllegalArgumentException("use a path inside the workspace, not an absolute path")
        }
        val segments = trimmed.replace('\\', '/').split('/').filter { it.isNotEmpty() && it != "." }
        if (segments.size >= 2 && segments[0] == LINKS_FOLDER) {
            val base = links[segments[1]]
                ?: throw IllegalArgumentException("there is no linked folder called ${segments[1]}")
            val rest = segments.drop(2).joinToString("/")
            return insideOrRefuse(base, File(base, rest.ifEmpty { "." }).canonicalFile)
        }
        return insideOrRefuse(root, File(root, trimmed.ifEmpty { "." }).canonicalFile)
    }

    private fun insideOrRefuse(base: File, candidate: File): File {
        val inside = candidate == base || candidate.path.startsWith(base.path + File.separator)
        if (!inside) throw IllegalArgumentException("that path leaves the workspace")
        return candidate
    }

    /** True for the folder that holds the linked folders, when there are any. */
    fun isLinksFolder(relative: String): Boolean =
        links.isNotEmpty() && relative.trim().replace('\\', '/').split('/').filter { it.isNotEmpty() && it != "." } == listOf(LINKS_FOLDER)

    /** The names of the linked folders, sorted. */
    fun linkNames(): List<String> = links.keys.sorted()

    /** The path of [file] relative to the workspace root, with forward slashes. Files in a linked folder start with [LINKS_FOLDER]. */
    fun relativePath(file: File): String {
        if (file == root) return "."
        val own = root.path + File.separator
        if (file.path.startsWith(own)) return file.path.removePrefix(own).replace(File.separatorChar, '/')
        for ((name, base) in links) {
            if (file == base) return "$LINKS_FOLDER/$name"
            val prefix = base.path + File.separator
            if (file.path.startsWith(prefix)) {
                return "$LINKS_FOLDER/$name/" + file.path.removePrefix(prefix).replace(File.separatorChar, '/')
            }
        }
        return file.path
    }

    /** The folders a search over the whole workspace walks: the root, then each linked folder. */
    fun searchRoots(): List<File> = listOf(root) + links.values

    /** The files directly in the workspace root that the tools may see. Secrets, partial copies and folders are left out. */
    fun files(): List<File> = root.listFiles().orEmpty()
        .filter { it.isFile && !it.name.endsWith(PART_SUFFIX) && !isSecret(it.name) }
        .sortedBy { it.name.lowercase() }

    /**
     * Stores [bytes] as [name] in the workspace root. The name must be a plain file name, the bytes must be text of at
     * most 2 MB, and no file may already have that name. The file is written in full or not at all.
     */
    fun store(name: String, bytes: ByteArray): File {
        require(!isSecret(name)) { "$name is a secret file, so it is not stored" }
        require(name.isNotEmpty() && name == safeName(name) && !name.endsWith(PART_SUFFIX)) {
            "that file name cannot be used"
        }
        require(bytes.size <= MAX_STORE_BYTES) { "$name is larger than 2 MB" }
        require(!bytes.contains(0)) { "$name is a binary file; the tools read text only" }
        val target = File(root, name)
        require(!target.exists()) { "A file called $name is already in the workspace." }
        root.mkdirs()
        val part = File(root, name + PART_SUFFIX)
        part.writeBytes(bytes)
        if (!part.renameTo(target)) {
            part.delete()
            throw IOException("$name could not be saved")
        }
        return target
    }

    /**
     * Copies [source] into the workspace root under a safe version of [name], for files the owner adds from this device. A
     * name already taken gets a number, so nothing is replaced. Binary files are kept, but the tools read only text. Stops
     * with an error once more than [maxBytes] have been read. Returns the name the file was saved under.
     */
    fun importFile(name: String, source: InputStream, maxBytes: Long): String {
        require(!isSecret(name)) { "$name is a secret file, so it is not added" }
        val plain = safeName(name)
        root.mkdirs()
        val saved = uniqueName(plain)
        val part = File(root, "$saved$PART_SUFFIX")
        try {
            var total = 0L
            part.outputStream().use { out ->
                val buffer = ByteArray(BUFFER_BYTES)
                while (true) {
                    val read = source.read(buffer)
                    if (read < 0) break
                    total += read
                    if (total > maxBytes) throw IOException("$name is too large to add here.")
                    out.write(buffer, 0, read)
                }
            }
            if (!part.renameTo(File(root, saved))) throw IOException("$name could not be saved")
        } finally {
            part.delete()
        }
        return saved
    }

    /** [name] if no file has it, otherwise "stem (2).ext", "stem (3).ext" and so on. */
    private fun uniqueName(name: String): String {
        if (!File(root, name).exists()) return name
        val dot = name.lastIndexOf('.').takeIf { it > 0 } ?: name.length
        val stem = name.substring(0, dot)
        val extension = name.substring(dot)
        var number = 2
        while (File(root, "$stem ($number)$extension").exists()) number++
        return "$stem ($number)$extension"
    }

    /**
     * Writes [bytes] to the file at [relative], creating the folders above it and replacing a file already there. The file
     * is written in full or not at all. Secrets, folders, binary data and anything over [MAX_WRITE_BYTES] are refused.
     * Returns the path as the workspace shows it.
     */
    fun write(relative: String, bytes: ByteArray): String {
        val target = resolve(relative)
        val shown = relativePath(target)
        require(shown != ".") { "give a file name inside the workspace" }
        require(!isSecret(shown)) { "$shown is a secret, so it is not written" }
        require(!target.isDirectory) { "$shown is a folder" }
        require(!bytes.contains(0)) { "$shown would be a binary file; the tools write text only" }
        require(bytes.size <= MAX_WRITE_BYTES) { "$shown is larger than 64 KB" }
        val parent = target.parentFile ?: throw IllegalArgumentException("that path cannot be written")
        parent.mkdirs()
        val part = File(parent, target.name + PART_SUFFIX)
        part.writeBytes(bytes)
        if (!part.renameTo(target)) {
            // Some systems do not replace a file on rename. The old copy goes, then the new one takes its place.
            val replaced = target.delete() && part.renameTo(target)
            if (!replaced) {
                part.delete()
                throw IOException("$shown could not be saved")
            }
        }
        return shown
    }

    /** Removes a file from the workspace root. False when there is no such file. */
    fun remove(name: String): Boolean {
        val target = File(root, name)
        return target.isFile && target.parentFile == root && target.delete()
    }

    companion object {
        private const val PART_SUFFIX = ".part"
        private const val BUFFER_BYTES = 8192
        const val MAX_STORE_BYTES = 2_000_000
        const val MAX_WRITE_BYTES = 64 * 1024

        /** The folder that holds the linked sandboxes. A folder of this name in the sandbox itself cannot be reached. */
        const val LINKS_FOLDER = "links"

        /** A plain file name made from [raw]: no folders, no leading dot, only letters, digits and `-_.` and spaces. */
        fun safeName(raw: String): String {
            val base = raw.substringAfterLast('/').substringAfterLast('\\').trim()
            val cleaned = base.map { if (it.isLetterOrDigit() || it == '-' || it == '_' || it == '.' || it == ' ') it else '_' }
                .joinToString("")
                .trimStart('.', ' ')
                .take(80)
                .trimEnd()
            return cleaned.ifEmpty { "file.txt" }
        }

        private val SECRET_NAMES = setOf(
            "control.json", "keys.json", "usage.json", "config.toml", ".env", ".netrc", "id_rsa", "id_ed25519",
            "credentials", "secrets.json", "token", "tokens.json",
        )
        private val SECRET_EXTENSIONS = setOf("pem", "key", "pfx", "p12", "kdbx", "env")
        private val SECRET_FOLDERS = setOf(".ssh", ".gnupg", ".aws", ".azure")

        /** True for files whose name, extension or folder marks them as secrets. Mirrors the desktop rule. */
        fun isSecret(relativePath: String): Boolean {
            val parts = relativePath.split('/', '\\').filter { it.isNotEmpty() }
            val name = parts.lastOrNull()?.lowercase().orEmpty()
            if (name in SECRET_NAMES || name.startsWith(".env.")) return true
            if (name.substringAfterLast('.', "") in SECRET_EXTENSIONS) return true
            return parts.any { it.lowercase() in SECRET_FOLDERS }
        }
    }
}
