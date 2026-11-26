package com.modelmistress.app

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import android.graphics.pdf.PdfRenderer
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import com.google.android.gms.tasks.Tasks
import com.google.mlkit.vision.common.InputImage
import com.google.mlkit.vision.label.defaults.ImageLabelerOptions
import com.google.mlkit.vision.label.ImageLabeling
import com.google.mlkit.vision.text.TextRecognition
import com.google.mlkit.vision.text.latin.TextRecognizerOptions
import java.io.File
import java.util.Locale
import java.util.UUID

/**
 * A file or photo added to a message. [label] is its name as the chat shows it. [text] is what the model reads: the file's
 * text, or what the photo shows. It is cut to [Attachments.MAX_CHARS] so it fits the model's context.
 */
data class Attachment(val label: String, val text: String)

/**
 * Reads what a document or a photo holds, on this phone. Text files are read as they are. PDFs are drawn page by page and read
 * with text recognition, since the phone has no PDF text reader. Pictures and photos get labels and any text in them from
 * ML Kit. None of it leaves the device.
 */
object Attachments {
    /** Most text one attachment adds to a message. About 500 tokens, so a few fit in the default context. */
    const val MAX_CHARS = 2000

    /** Pages of a PDF that are read. The first pages hold most of the text in a short document. */
    const val MAX_PDF_PAGES = 3

    /** Longest side of a picture that recognition reads. Larger pictures are scaled down first. */
    private const val MAX_IMAGE_SIDE = 1280

    /** Bytes read from a text file. Anything past this is not sent to the model anyway. */
    private const val MAX_READ_BYTES = 256 * 1024

    private val TEXT_EXTENSIONS = setOf(
        "txt", "md", "markdown", "csv", "tsv", "json", "xml", "yaml", "yml", "log", "html", "htm", "ini", "conf", "toml",
        "properties", "kt", "java", "py", "js", "ts", "c", "h", "cpp", "rs", "go", "sh", "sql",
    )

    /** Reads a document the owner picked. Throws [IllegalArgumentException] with a reason the chat can show. */
    fun fromDocument(context: Context, uri: Uri): Attachment {
        val name = displayName(context, uri)
        val extension = name.substringAfterLast('.', "").lowercase(Locale.US)
        val mime = context.contentResolver.getType(uri).orEmpty()
        return when {
            extension == "pdf" || mime == "application/pdf" -> {
                val file = copyToCache(context, uri, "pdf")
                val text = try { pdfText(file) } finally { file.delete() }
                Attachment(name, "PDF text from the first pages:\n" + clip(text))
            }
            mime.startsWith("image/") || extension in IMAGE_EXTENSIONS -> {
                val file = copyToCache(context, uri, extension.ifEmpty { "jpg" })
                val description = try { describePicture(file, "Picture") } finally { file.delete() }
                Attachment(name, description)
            }
            mime.startsWith("text/") || extension in TEXT_EXTENSIONS || mime == "application/json" -> {
                val bytes = context.contentResolver.openInputStream(uri)?.use { stream ->
                    readAtMost(stream, MAX_READ_BYTES)
                } ?: throw IllegalArgumentException("$name could not be opened.")
                if (bytes.contains(0.toByte())) throw IllegalArgumentException("$name is not a text file.")
                Attachment(name, clip(String(bytes, Charsets.UTF_8)))
            }
            else -> throw IllegalArgumentException(
                "$name is not supported yet. Text files, PDFs and pictures can be attached.",
            )
        }
    }

    /** Describes a photo the camera saved to [file]. */
    fun fromPhoto(file: File): Attachment = Attachment("Photo", describePicture(file, "Photo"))

    /** Cuts [text] to [MAX_CHARS], with a note that it was cut. */
    fun clip(text: String): String {
        val trimmed = text.trim()
        if (trimmed.length <= MAX_CHARS) return trimmed
        return trimmed.take(MAX_CHARS) + "\n[The rest was cut to fit the model's context.]"
    }

    private fun describePicture(file: File, what: String): String {
        val bitmap = decodeScaled(file) ?: throw IllegalArgumentException("The picture could not be read.")
        try {
            return "$what: " + recognise(bitmap)
        } finally {
            bitmap.recycle()
        }
    }

    /** Labels and text for one picture. */
    private fun recognise(bitmap: Bitmap): String {
        val image = InputImage.fromBitmap(bitmap, 0)
        val labeler = ImageLabeling.getClient(ImageLabelerOptions.Builder().setConfidenceThreshold(LABEL_CONFIDENCE).build())
        val recognizer = TextRecognition.getClient(TextRecognizerOptions.DEFAULT_OPTIONS)
        try {
            val labels = Tasks.await(labeler.process(image)).sortedByDescending { it.confidence }.take(MAX_LABELS)
            val text = Tasks.await(recognizer.process(image)).text.trim()
            val labelText = if (labels.isEmpty()) {
                "no labels recognised"
            } else {
                labels.joinToString(", ") { "${it.text} (${(it.confidence * 100).toInt()}%)" }
            }
            return buildString {
                append("Labels: ").append(labelText).append('.')
                if (text.isNotEmpty()) append("\nText in the picture:\n").append(clip(text))
            }
        } catch (error: Exception) {
            throw IllegalArgumentException(
                "Recognition needs Google Play services on this phone (${error.message ?: "unavailable"}).",
            )
        } finally {
            labeler.close()
            recognizer.close()
        }
    }

    private fun readAtMost(stream: java.io.InputStream, limit: Int): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        val buffer = ByteArray(8192)
        while (out.size() < limit) {
            val read = stream.read(buffer, 0, minOf(buffer.size, limit - out.size()))
            if (read < 0) break
            out.write(buffer, 0, read)
        }
        return out.toByteArray()
    }

    private fun pdfText(file: File): String {
        val descriptor = ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
        descriptor.use {
            PdfRenderer(descriptor).use { renderer ->
                if (renderer.pageCount == 0) return ""
                val pages = minOf(renderer.pageCount, MAX_PDF_PAGES)
                val recognizer = TextRecognition.getClient(TextRecognizerOptions.DEFAULT_OPTIONS)
                try {
                    val parts = (0 until pages).mapNotNull { index ->
                        renderer.openPage(index).use { page ->
                            val width = PAGE_WIDTH
                            val height = (width * page.height.toFloat() / page.width.toFloat()).toInt().coerceAtLeast(1)
                            val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
                            bitmap.eraseColor(Color.WHITE)
                            page.render(bitmap, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
                            try {
                                Tasks.await(recognizer.process(InputImage.fromBitmap(bitmap, 0))).text.trim()
                                    .takeIf { it.isNotEmpty() }
                            } finally {
                                bitmap.recycle()
                            }
                        }
                    }
                    return parts.joinToString("\n\n").ifEmpty { "No text was found on the first pages." }
                } catch (error: Exception) {
                    throw IllegalArgumentException(
                        "The PDF's text could not be read: ${error.message ?: "recognition unavailable"}.",
                    )
                } finally {
                    recognizer.close()
                }
            }
        }
    }

    private fun decodeScaled(file: File): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(file.absolutePath, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / sample > MAX_IMAGE_SIDE) sample *= 2
        return BitmapFactory.decodeFile(file.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
    }

    private fun copyToCache(context: Context, uri: Uri, extension: String): File {
        val folder = File(context.cacheDir, "attachments").apply { mkdirs() }
        val file = File(folder, "${UUID.randomUUID()}.$extension")
        val input = context.contentResolver.openInputStream(uri)
            ?: throw IllegalArgumentException("The file could not be opened.")
        input.use { source -> file.outputStream().use { target -> source.copyTo(target) } }
        return file
    }

    private fun displayName(context: Context, uri: Uri): String {
        val fromProvider = context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { cursor -> if (cursor.moveToFirst()) cursor.getString(0) else null }
        return fromProvider?.takeIf { it.isNotBlank() } ?: uri.lastPathSegment?.substringAfterLast('/') ?: "file"
    }

    private val IMAGE_EXTENSIONS = setOf("jpg", "jpeg", "png", "webp", "heic", "bmp", "gif")
    private const val LABEL_CONFIDENCE = 0.5f
    private const val MAX_LABELS = 6
    private const val PAGE_WIDTH = 1200
}
