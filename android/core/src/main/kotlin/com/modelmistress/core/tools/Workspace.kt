package com.modelmistress.core.tools

import java.io.File

/**
 * The folder the file tools may read. Every path is resolved inside [root]: absolute paths, `..` and symbolic links that
 * leave the folder are refused. Files with secrets in their names, extensions or folders are never read or listed.
 */
class Workspace(root: File) {
    val root: File = root.canonicalFile

    /** The file or folder a path names, or an error when the path leaves the workspace. An empty path is the root. */
    fun resolve(relative: String): File {
        val trimmed = relative.trim()
        if (trimmed.startsWith("/") || trimmed.startsWith("\\") || (trimmed.length >= 2 && trimmed[1] == ':')) {
            throw IllegalArgumentException("use a path inside the workspace, not an absolute path")
        }
        val candidate = File(root, trimmed.ifEmpty { "." }).canonicalFile
        val inside = candidate == root || candidate.path.startsWith(root.path + File.separator)
        if (!inside) throw IllegalArgumentException("that path leaves the workspace")
        return candidate
    }

    /** The path of [file] relative to the workspace root, with forward slashes. */
    fun relativePath(file: File): String {
        if (file == root) return "."
        return file.path.removePrefix(root.path + File.separator).replace(File.separatorChar, '/')
    }

    /** The files directly in the workspace that the tools may see. Secrets, partial copies and folders are left out. */
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
            throw java.io.IOException("$name could not be saved")
        }
        return target
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
                throw java.io.IOException("$shown could not be saved")
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
        const val MAX_STORE_BYTES = 2_000_000
        const val MAX_WRITE_BYTES = 64 * 1024

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
