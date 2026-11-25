package com.modelmistress.runtime

import android.content.ContentResolver
import android.net.Uri
import android.provider.OpenableColumns
import com.modelmistress.core.gguf.GgufInfo
import com.modelmistress.core.gguf.GgufReader
import java.io.File
import java.io.IOException

/** A GGUF model file in the app's own storage. [info] is null when its header cannot be read. */
data class LocalModel(
    val id: String,
    val file: File,
    val sizeBytes: Long,
    val info: GgufInfo?,
) {
    /** Architecture and quantisation, for example "qwen2 · Q4_K_M". */
    val summary: String
        get() = listOfNotNull(info?.architecture, info?.quantization).joinToString(" · ").ifEmpty { "GGUF model" }
}

/**
 * The GGUF models this device keeps. Files are copied in through the system file picker, so the app never needs storage
 * permission and never reads another app's files.
 */
class ModelLibrary(private val directory: File) {
    init {
        directory.mkdirs()
    }

    fun list(): List<LocalModel> = directory.listFiles { file -> file.isFile && file.name.endsWith(EXTENSION) }
        .orEmpty()
        .sortedBy { it.name }
        .map(::describe)

    fun find(id: String): LocalModel? = fileFor(id)?.takeIf { it.isFile }?.let(::describe)

    fun delete(id: String): Boolean = fileFor(id)?.delete() ?: false

    private fun fileFor(id: String): File? = if (ID_PATTERN.matches(id)) File(directory, id + EXTENSION) else null

    /** Copies the file behind [uri] into the library. A file that is not a GGUF model is refused and not kept. */
    fun import(resolver: ContentResolver, uri: Uri): LocalModel {
        val target = uniqueFile(sanitize(displayName(resolver, uri).removeSuffix(EXTENSION)))
        val partial = File(directory, target.name + PARTIAL)
        try {
            val source = resolver.openInputStream(uri) ?: throw IOException("The file could not be opened.")
            source.use { input -> partial.outputStream().use { output -> input.copyTo(output) } }
            try {
                GgufReader.read(partial)
            } catch (error: Exception) {
                throw IllegalArgumentException("That file is not a readable GGUF model.", error)
            }
            if (!partial.renameTo(target)) throw IOException("The model could not be saved.")
        } catch (error: Exception) {
            partial.delete()
            throw error
        }
        return describe(target)
    }

    private fun describe(file: File): LocalModel = LocalModel(
        id = file.name.removeSuffix(EXTENSION),
        file = file,
        sizeBytes = file.length(),
        info = runCatching { GgufReader.read(file) }.getOrNull(),
    )

    private fun displayName(resolver: ContentResolver, uri: Uri): String {
        val fromProvider = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        }
        return fromProvider ?: uri.lastPathSegment?.substringAfterLast('/') ?: "model"
    }

    private fun uniqueFile(base: String): File {
        var candidate = File(directory, base + EXTENSION)
        var number = 2
        while (candidate.exists()) {
            candidate = File(directory, "$base-$number$EXTENSION")
            number++
        }
        return candidate
    }

    private fun sanitize(name: String): String =
        name.replace(Regex("[^A-Za-z0-9._-]+"), "-").trim('-', '.').take(MAX_NAME).ifEmpty { "model" }

    companion object {
        const val EXTENSION = ".gguf"
        private const val PARTIAL = ".part"
        private const val MAX_NAME = 64
        private val ID_PATTERN = Regex("[A-Za-z0-9._-]{1,$MAX_NAME}")
    }
}
