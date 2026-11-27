package com.modelmistress.core.models

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.security.MessageDigest

/** Raised when the owner stops a download. What was fetched is kept, so the next attempt resumes it. */
class DownloadStopped : IOException("The download was stopped. Start it again to resume.")

/**
 * Downloads one file into [partial] and checks it. A file that was already part-way is resumed from where it stopped.
 * A file that does not match its size or checksum is thrown away, so a bad copy never becomes a model.
 */
object FileDownloader {
    private const val BUFFER_BYTES = 64 * 1024

    /**
     * Fetches [url] into [partial]. [expectedSize] and [expectedSha256] come from the repo listing; a zero size or a null
     * checksum skips that check. [progress] gets the bytes held so far and the expected total. [keepGoing] is asked before
     * each chunk; returning false stops the download with [DownloadStopped], and the bytes fetched so far stay in [partial].
     */
    fun fetch(
        client: HuggingFaceClient,
        url: String,
        expectedSize: Long,
        expectedSha256: String?,
        partial: File,
        progress: (done: Long, total: Long) -> Unit,
        keepGoing: () -> Boolean,
    ): File {
        var held = if (partial.isFile) partial.length() else 0L
        if (expectedSize > 0 && held > expectedSize) {
            partial.delete()
            held = 0L
        }
        if (!(expectedSize > 0 && held == expectedSize)) {
            transfer(client, url, expectedSize, partial, held, progress, keepGoing)
        }
        if (expectedSize > 0 && partial.length() != expectedSize) {
            throw IOException("The download stopped early. Start it again to resume.")
        }
        if (expectedSha256 != null && sha256Of(partial) != expectedSha256.lowercase()) {
            partial.delete()
            throw IOException("The download did not match its checksum, so it was thrown away. Start it again.")
        }
        progress(partial.length(), expectedSize)
        return partial
    }

    private fun transfer(
        client: HuggingFaceClient,
        url: String,
        expectedSize: Long,
        partial: File,
        startAt: Long,
        progress: (done: Long, total: Long) -> Unit,
        keepGoing: () -> Boolean,
    ) {
        var connection = client.connect(url, if (startAt > 0) startAt else null)
        var status = connection.responseCode
        if (status == HTTP_RANGE_NOT_SATISFIABLE && startAt > 0) {
            // The part on disk no longer fits what the server has. Start the file again.
            connection.disconnect()
            partial.delete()
            connection = client.connect(url, null)
            status = connection.responseCode
        }
        try {
            when {
                status == 401 || status == 403 -> throw HfException(
                    "Hugging Face refused this file. A gated or private repo needs a Hugging Face token in Settings, " +
                        "and the account must have accepted the repo's terms.",
                )
                status == 404 -> throw HfException("This file is no longer in the repo.")
                status >= 400 -> throw IOException("Hugging Face answered $status for this file.")
            }
            val resuming = status == HTTP_PARTIAL
            val held = if (resuming) startAt else 0L
            var done = held
            connection.inputStream.use { input ->
                FileOutputStream(partial, resuming).use { output ->
                    val buffer = ByteArray(BUFFER_BYTES)
                    while (true) {
                        if (!keepGoing()) throw DownloadStopped()
                        val read = input.read(buffer)
                        if (read < 0) break
                        output.write(buffer, 0, read)
                        done += read
                        progress(done, expectedSize)
                    }
                }
            }
        } finally {
            connection.disconnect()
        }
    }

    /** The SHA-256 of [file] as lowercase hex. */
    fun sha256Of(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buffer = ByteArray(BUFFER_BYTES)
            while (true) {
                val read = input.read(buffer)
                if (read < 0) break
                digest.update(buffer, 0, read)
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    private const val HTTP_PARTIAL = 206
    private const val HTTP_RANGE_NOT_SATISFIABLE = 416
}
