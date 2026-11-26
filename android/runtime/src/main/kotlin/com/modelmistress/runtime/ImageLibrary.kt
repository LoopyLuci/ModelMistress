package com.modelmistress.runtime

import android.content.ContentResolver
import android.net.Uri
import android.provider.OpenableColumns
import java.io.File
import java.io.IOException

/** A Stable Diffusion checkpoint in the app's own storage. The id is its name without the extension. */
data class LocalImageModel(
    val id: String,
    val file: File,
    val sizeBytes: Long,
)

/**
 * The image models this device keeps: single-file Stable Diffusion checkpoints, `.safetensors` or `.ckpt`. Like the chat
 * models, they are copied in through the system file picker, so the app never reads another app's files.
 */
class ImageLibrary(private val directory: File) {
    init {
        directory.mkdirs()
    }

    fun list(): List<LocalImageModel> = directory.listFiles { file -> file.isFile && extensionOf(file.name) != null }
        .orEmpty()
        .sortedBy { it.name }
        .map(::describe)

    fun find(id: String): LocalImageModel? = fileFor(id)?.let(::describe)

    fun delete(id: String): Boolean = fileFor(id)?.delete() ?: false

    /**
     * Copies the file behind [uri] into the library. A name that is not a checkpoint, and an empty file, are refused and not
     * kept.
     */
    fun import(resolver: ContentResolver, uri: Uri): LocalImageModel {
        val name = displayName(resolver, uri)
        val extension = extensionOf(name)
            ?: throw IllegalArgumentException("Choose a .safetensors or .ckpt Stable Diffusion checkpoint.")
        val stem = uniqueStem(sanitize(name.substring(0, name.length - extension.length)))
        val target = File(directory, stem + extension)
        val partial = File(directory, target.name + PARTIAL)
        try {
            val source = resolver.openInputStream(uri) ?: throw IOException("The file could not be opened.")
            source.use { input -> partial.outputStream().use { output -> input.copyTo(output) } }
            if (partial.length() == 0L) throw IllegalArgumentException("The file is empty.")
            if (!partial.renameTo(target)) throw IOException("The model could not be saved.")
        } catch (error: Exception) {
            partial.delete()
            throw error
        }
        return describe(target)
    }

    private fun fileFor(id: String): File? {
        if (!ID_PATTERN.matches(id)) return null
        return EXTENSIONS.map { File(directory, id + it) }.firstOrNull { it.isFile }
    }

    /** True when a checkpoint called [name] is already installed, with any extension. */
    private fun taken(name: String): Boolean = EXTENSIONS.any { File(directory, name + it).exists() }

    private fun uniqueStem(base: String): String {
        var stem = base
        var number = 2
        while (taken(stem)) {
            stem = "$base-$number"
            number++
        }
        return stem
    }

    private fun describe(file: File): LocalImageModel {
        val extension = checkNotNull(extensionOf(file.name)) { "${file.name} is not a checkpoint." }
        return LocalImageModel(
            id = file.name.substring(0, file.name.length - extension.length),
            file = file,
            sizeBytes = file.length(),
        )
    }

    private fun extensionOf(name: String): String? {
        val lower = name.lowercase()
        return EXTENSIONS.firstOrNull { lower.endsWith(it) }
    }

    private fun displayName(resolver: ContentResolver, uri: Uri): String {
        val fromProvider = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        }
        return fromProvider ?: uri.lastPathSegment?.substringAfterLast('/') ?: "model"
    }

    private fun sanitize(name: String): String =
        name.replace(Regex("[^A-Za-z0-9._-]+"), "-").trim('-', '.').take(MAX_NAME).ifEmpty { "model" }

    companion object {
        private val EXTENSIONS = listOf(".safetensors", ".ckpt")
        private const val PARTIAL = ".part"
        private const val MAX_NAME = 64
        private val ID_PATTERN = Regex("[A-Za-z0-9._-]{1,$MAX_NAME}")
    }
}
