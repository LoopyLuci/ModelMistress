package com.modelmistress.runtime

import android.content.ContentResolver
import android.net.Uri
import android.provider.OpenableColumns
import com.modelmistress.core.gguf.GgufInfo
import com.modelmistress.core.gguf.GgufReader
import com.modelmistress.core.models.ModelFormat
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream

/** Room a received model must leave free on the device, so the device keeps working. */
private const val ROOM_TO_SPARE = 512L * 1024 * 1024

/** The device has no room for a model that was sent to it. */
class NoRoom(message: String) : IOException(message)

/**
 * A model file in the app's own storage, as GGUF (llama.cpp) or LiteRT-LM. [info] is the GGUF header. It is null for
 * LiteRT-LM models, and for a GGUF file whose header cannot be read.
 */
data class LocalModel(
    val id: String,
    val file: File,
    val format: ModelFormat,
    val sizeBytes: Long,
    val info: GgufInfo?,
) {
    /** Architecture and quantisation for a GGUF model, for example "qwen2 · Q4_K_M". */
    val summary: String
        get() = when (format) {
            ModelFormat.GGUF -> listOfNotNull(info?.architecture, info?.quantization)
                .joinToString(" · ")
                .ifEmpty { "GGUF model" }
            ModelFormat.LITERT_LM -> "LiteRT-LM model"
        }
}

/**
 * The models this device keeps: GGUF files for llama.cpp and LiteRT-LM files. Files are copied in through the system file
 * picker, so the app never needs storage permission and never reads another app's files.
 */
class ModelLibrary(private val directory: File) {
    init {
        directory.mkdirs()
    }

    fun list(): List<LocalModel> = directory.listFiles { file -> file.isFile && ModelFormat.of(file.name) != null }
        .orEmpty()
        .sortedBy { it.name }
        .map(::describe)

    fun find(id: String): LocalModel? = fileFor(id)?.let(::describe)

    fun delete(id: String): Boolean = fileFor(id)?.delete() ?: false

    /** The model a download named [stem] will become. Downloads are GGUF only. */
    fun targetFor(stem: String): File = File(directory, sanitize(stem) + ModelFormat.GGUF.extension)

    /** Where a download named [stem] is kept while it arrives. It is not listed until [install] moves it in. */
    fun partialFor(stem: String): File = File(directory, sanitize(stem) + ModelFormat.GGUF.extension + PARTIAL)

    /** Free space on the device, for a download to check before it starts. */
    fun usableBytes(): Long = directory.usableSpace

    /**
     * Moves a finished download into the library as the model [stem]. A file that is not a readable GGUF model is deleted
     * and refused, and a model that is already here is not replaced.
     */
    fun install(partial: File, stem: String): LocalModel {
        val name = sanitize(stem)
        if (taken(name)) throw IllegalArgumentException("$name is already in the library.")
        try {
            GgufReader.read(partial)
        } catch (error: Exception) {
            partial.delete()
            throw IllegalArgumentException("The download is not a readable GGUF model.", error)
        }
        val target = File(directory, name + ModelFormat.GGUF.extension)
        if (!partial.renameTo(target)) throw IOException("The model could not be saved.")
        return describe(target)
    }

    /**
     * Copies the file behind [uri] into the library. A name ending in `.litertlm` is kept as LiteRT-LM. Any other name is
     * kept as GGUF, and a file that is not a readable GGUF model is refused and not kept.
     */
    fun import(resolver: ContentResolver, uri: Uri): LocalModel {
        val name = displayName(resolver, uri)
        val format = ModelFormat.of(name) ?: ModelFormat.GGUF
        val stem = uniqueStem(sanitize(name.removeSuffix(format.extension)))
        val target = File(directory, stem + format.extension)
        val partial = File(directory, target.name + PARTIAL)
        try {
            val source = resolver.openInputStream(uri) ?: throw IOException("The file could not be opened.")
            source.use { input -> partial.outputStream().use { output -> input.copyTo(output) } }
            if (partial.length() == 0L) throw IllegalArgumentException("The file is empty.")
            if (format == ModelFormat.GGUF) {
                try {
                    GgufReader.read(partial)
                } catch (error: Exception) {
                    throw IllegalArgumentException("That file is not a readable GGUF model.", error)
                }
            }
            if (!partial.renameTo(target)) throw IOException("The model could not be saved.")
        } catch (error: Exception) {
            partial.delete()
            throw error
        }
        return describe(target)
    }

    /**
     * Takes a model that another ModelMistress sent: exactly [bytes] of it are copied from [body] into the library, under
     * the name it was sent with. Refused: a name that is not a .gguf file, an empty file, a copy cut short, a file that is
     * not a readable GGUF model, and a copy that would leave less than [ROOM_TO_SPARE] free. A model already here with the
     * same name is not replaced: a number is added to the new one.
     */
    fun receive(fileName: String, body: InputStream, bytes: Long): LocalModel {
        val format = ModelFormat.of(fileName)
        if (format != ModelFormat.GGUF) throw IllegalArgumentException("Only .gguf model files can be sent here.")
        if (bytes <= 0L) throw IllegalArgumentException("The model file is empty.")
        if (usableBytes() < bytes + ROOM_TO_SPARE) {
            throw NoRoom("The device has ${usableBytes() / 1_000_000} MB free; the model needs ${bytes / 1_000_000} MB and some room to spare.")
        }
        val stem = uniqueStem(sanitize(fileName.removeSuffix(format.extension)))
        val target = File(directory, stem + format.extension)
        val partial = File(directory, target.name + PARTIAL)
        try {
            partial.outputStream().use { output -> copyExactly(body, output, bytes) }
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

    /** Copies exactly [bytes] bytes; a stream that ends before them is an error, and nothing more is read. */
    private fun copyExactly(input: InputStream, output: OutputStream, bytes: Long) {
        val buffer = ByteArray(1 shl 20)
        var left = bytes
        while (left > 0) {
            val n = input.read(buffer, 0, minOf(buffer.size.toLong(), left).toInt())
            if (n < 0) throw IOException("The model arrived cut short: ${bytes - left} of $bytes bytes.")
            output.write(buffer, 0, n)
            left -= n
        }
    }

    /** The installed file for [id], in whichever format it is. Null when there is none. */
    private fun fileFor(id: String): File? {
        if (!ID_PATTERN.matches(id)) return null
        return ModelFormat.entries.map { File(directory, id + it.extension) }.firstOrNull { it.isFile }
    }

    /** True when a model called [name] is already installed, in any format. */
    private fun taken(name: String): Boolean =
        ModelFormat.entries.any { File(directory, name + it.extension).exists() }

    private fun uniqueStem(base: String): String {
        var stem = base
        var number = 2
        while (taken(stem)) {
            stem = "$base-$number"
            number++
        }
        return stem
    }

    private fun describe(file: File): LocalModel {
        val format = checkNotNull(ModelFormat.of(file.name)) { "${file.name} is not a model file." }
        return LocalModel(
            id = file.name.removeSuffix(format.extension),
            file = file,
            format = format,
            sizeBytes = file.length(),
            info = if (format == ModelFormat.GGUF) runCatching { GgufReader.read(file) }.getOrNull() else null,
        )
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
        private const val PARTIAL = ".part"
        private const val MAX_NAME = 64
        private val ID_PATTERN = Regex("[A-Za-z0-9._-]{1,$MAX_NAME}")
    }
}
