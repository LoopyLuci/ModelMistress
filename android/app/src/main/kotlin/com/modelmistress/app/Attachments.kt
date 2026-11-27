package com.modelmistress.app

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import android.graphics.Matrix
import android.graphics.pdf.PdfRenderer
import android.media.ExifInterface
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import com.google.android.gms.tasks.Tasks
import com.google.mlkit.vision.common.InputImage
import com.google.mlkit.vision.label.defaults.ImageLabelerOptions
import com.google.mlkit.vision.label.ImageLabeling
import com.google.mlkit.vision.text.TextRecognition
import com.google.mlkit.vision.text.latin.TextRecognizerOptions
import com.modelmistress.core.chat.AttachmentKind
import com.modelmistress.core.chat.ChatAttachment
import java.io.File
import java.io.InputStream
import java.util.Locale

/**
 * Reads what a document or a picture holds, on this phone. Text files are read as they are. PDFs are drawn page by page and read
 * with text recognition, since the phone has no PDF text reader. Pictures get labels and any text in them from ML Kit. None of
 * it leaves the device.
 *
 * The file itself is kept in the chat's sandbox, so the chat can show it and open it. Only the text read from it goes to the model.
 */
object Attachments {
    /** Most text one attachment adds to a message. About 500 tokens, so a few fit in the default context. */
    const val MAX_CHARS = 2000

    /** Pages of a PDF that are read. The first pages hold most of the text in a short document. */
    const val MAX_PDF_PAGES = 3

    /** Largest side of a picture that recognition reads. Larger pictures are scaled down first. */
    private const val MAX_IMAGE_SIDE = 1280

    /** Bytes read from a text file. Anything past this is not sent to the model anyway. */
    private const val MAX_READ_BYTES = 256 * 1024

    /** Largest file the owner can attach or add to a sandbox. */
    const val MAX_FILE_BYTES = 25L * 1024 * 1024

    private val TEXT_EXTENSIONS = setOf(
        "txt", "md", "markdown", "csv", "tsv", "json", "xml", "yaml", "yml", "log", "html", "htm", "ini", "conf", "toml",
        "properties", "kt", "java", "py", "js", "ts", "c", "h", "cpp", "rs", "go", "sh", "sql",
    )
    private val IMAGE_EXTENSIONS = setOf("jpg", "jpeg", "png", "webp", "heic", "bmp", "gif")
    private const val LABEL_CONFIDENCE = 0.5f
    private const val MAX_LABELS = 6
    private const val PAGE_WIDTH = 1200

    /** True when the chat can read a file of this name and type. Others are kept in the sandbox but not attached. */
    fun supports(name: String, mime: String): Boolean = kindOf(name, mime) != null

    /** The kind of attachment a file makes, or null when the chat cannot read it. */
    fun kindOf(name: String, mime: String): AttachmentKind? {
        val extension = extensionOf(name)
        return when {
            extension == "pdf" || mime == "application/pdf" -> AttachmentKind.DOCUMENT
            mime.startsWith("image/") || extension in IMAGE_EXTENSIONS -> AttachmentKind.PICTURE
            mime.startsWith("text/") || extension in TEXT_EXTENSIONS || mime == "application/json" -> AttachmentKind.DOCUMENT
            else -> null
        }
    }

    /**
     * Reads a file the sandbox has stored as [stored], and returns the attachment for it, labelled [label]. Throws
     * [IllegalArgumentException] with a reason the chat can show when the file cannot be read.
     */
    fun read(stored: File, label: String, mime: String): ChatAttachment {
        val kind = kindOf(stored.name, mime)
            ?: throw IllegalArgumentException("$label is not supported yet. Text files, PDFs and pictures can be attached.")
        val text = when {
            extensionOf(stored.name) == "pdf" || mime == "application/pdf" -> "PDF text from the first pages:\n" + clip(pdfText(stored))
            kind == AttachmentKind.PICTURE -> pictureText(stored)
            else -> {
                val bytes = stored.inputStream().use { readAtMost(it, MAX_READ_BYTES) }
                if (bytes.contains(0.toByte())) throw IllegalArgumentException("$label is not a text file.")
                clip(String(bytes, Charsets.UTF_8))
            }
        }
        return ChatAttachment(label = label, file = stored.name, kind = kind, text = text)
    }

    /** Labels and text for a bitmap from the camera, turned by [rotationDegrees] so ML Kit sees it upright. */
    fun describeBitmap(bitmap: Bitmap, rotationDegrees: Int): String = recognise(bitmap, rotationDegrees)

    /** The file name a picked document has on the device, or a plain fallback. */
    fun nameOf(context: Context, uri: Uri): String {
        val fromProvider = context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { cursor -> if (cursor.moveToFirst()) cursor.getString(0) else null }
        return fromProvider?.takeIf { it.isNotBlank() } ?: uri.lastPathSegment?.substringAfterLast('/') ?: "file"
    }

    /** The type a file opens as. Pictures and PDFs go to the apps that show them; anything else is text. */
    fun mimeFor(name: String): String {
        val extension = extensionOf(name)
        return when {
            extension == "pdf" -> "application/pdf"
            extension in IMAGE_EXTENSIONS -> "image/*"
            extension == "json" -> "application/json"
            extension in TEXT_EXTENSIONS -> "text/plain"
            else -> "*/*"
        }
    }

    /** Cuts [text] to [MAX_CHARS], with a note that it was cut. */
    fun clip(text: String): String {
        val trimmed = text.trim()
        if (trimmed.length <= MAX_CHARS) return trimmed
        return trimmed.take(MAX_CHARS) + "\n[The rest was cut to fit the model's context.]"
    }

    private fun pictureText(file: File): String {
        // decodeScaled has already turned the picture upright, so recognition reads it with no further turn.
        val bitmap = decodeScaled(file) ?: throw IllegalArgumentException("The picture could not be read.")
        try {
            return "Picture: " + recognise(bitmap, 0)
        } finally {
            bitmap.recycle()
        }
    }

    /** Labels and text for one picture, turned upright by [rotationDegrees]. */
    private fun recognise(bitmap: Bitmap, rotationDegrees: Int): String {
        val image = InputImage.fromBitmap(bitmap, rotationDegrees)
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

    private fun readAtMost(stream: InputStream, limit: Int): ByteArray {
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

    /** Degrees a photo must turn to be upright, from its EXIF orientation. Zero when the file says nothing. */
    fun rotationOf(file: File): Int {
        val orientation = runCatching { ExifInterface(file.absolutePath).getAttributeInt(ExifInterface.TAG_ORIENTATION, 1) }
            .getOrDefault(ExifInterface.ORIENTATION_NORMAL)
        return when (orientation) {
            ExifInterface.ORIENTATION_ROTATE_90 -> 90
            ExifInterface.ORIENTATION_ROTATE_180 -> 180
            ExifInterface.ORIENTATION_ROTATE_270 -> 270
            else -> 0
        }
    }

    /** A picture scaled down so its longest side is at most [MAX_IMAGE_SIDE], turned upright. Null when it cannot be read. */
    fun decodeScaled(file: File, maxSide: Int = MAX_IMAGE_SIDE): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(file.absolutePath, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / sample > maxSide) sample *= 2
        val decoded = BitmapFactory.decodeFile(file.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
            ?: return null
        // The thumbnails and labels are read upright; the copy is turned here, so both views agree.
        val turn = rotationOf(file)
        if (turn == 0) return decoded
        val matrix = Matrix().apply { postRotate(turn.toFloat()) }
        val upright = Bitmap.createBitmap(decoded, 0, 0, decoded.width, decoded.height, matrix, true)
        if (upright != decoded) decoded.recycle()
        return upright
    }

    private fun extensionOf(name: String): String = name.substringAfterLast('.', "").lowercase(Locale.US)
}
