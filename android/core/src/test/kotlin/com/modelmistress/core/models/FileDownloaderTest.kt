package com.modelmistress.core.models

import com.sun.net.httpserver.HttpServer
import java.io.File
import java.io.IOException
import java.net.InetAddress
import java.net.InetSocketAddress
import java.nio.file.Files
import java.security.MessageDigest
import java.util.concurrent.CopyOnWriteArrayList
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test

/** Downloads against a small local server that serves ranges, redirects and a gated file, like Hugging Face does. */
class FileDownloaderTest {
    private val data = ByteArray(300_000) { (it * 31 % 251).toByte() }
    private val sha = MessageDigest.getInstance("SHA-256").digest(data).joinToString("") { "%02x".format(it) }
    private val seenAuthorization = CopyOnWriteArrayList<String?>()
    private lateinit var server: HttpServer
    private lateinit var base: String
    private lateinit var folder: File

    @Before
    fun start() {
        server = HttpServer.create(InetSocketAddress(InetAddress.getLoopbackAddress(), 0), 0)
        server.createContext("/file") { exchange ->
            seenAuthorization += exchange.requestHeaders.getFirst("Authorization")
            val range = exchange.requestHeaders.getFirst("Range")
            if (range == null) {
                exchange.sendResponseHeaders(200, data.size.toLong())
                exchange.responseBody.use { it.write(data) }
            } else {
                val start = Regex("bytes=(\\d+)-").find(range)!!.groupValues[1].toInt()
                val rest = data.copyOfRange(start, data.size)
                exchange.responseHeaders.add("Content-Range", "bytes $start-${data.size - 1}/${data.size}")
                exchange.sendResponseHeaders(206, rest.size.toLong())
                exchange.responseBody.use { it.write(rest) }
            }
            exchange.close()
        }
        server.createContext("/redirect") { exchange ->
            exchange.responseHeaders.add("Location", "/file")
            exchange.sendResponseHeaders(302, -1)
            exchange.close()
        }
        server.createContext("/gated") { exchange ->
            seenAuthorization += exchange.requestHeaders.getFirst("Authorization")
            exchange.sendResponseHeaders(401, -1)
            exchange.close()
        }
        server.createContext("/api/models/org/repo/revision/main") { exchange ->
            val body = REPO_LISTING_JSON.toByteArray(Charsets.UTF_8)
            exchange.responseHeaders.add("Content-Type", "application/json")
            exchange.sendResponseHeaders(200, body.size.toLong())
            exchange.responseBody.use { it.write(body) }
            exchange.close()
        }
        server.start()
        base = "http://127.0.0.1:${server.address.port}"
        folder = Files.createTempDirectory("downloads").toFile()
    }

    @After
    fun stop() {
        server.stop(0)
        folder.deleteRecursively()
    }

    private fun client(token: String? = null) = HuggingFaceClient(token, base)

    private fun partial(name: String = "model.gguf.part") = File(folder, name)

    @Test
    fun downloadsAWholeFileAndChecksIt() {
        val progress = CopyOnWriteArrayList<Long>()
        val result = FileDownloader.fetch(
            client(), "$base/file", data.size.toLong(), sha, partial(), { done, _ -> progress += done }, { true },
        )
        assertEquals(data.size.toLong(), result.length())
        assertEquals(sha, FileDownloader.sha256Of(result))
        assertEquals(data.size.toLong(), progress.last())
    }

    @Test
    fun resumesAPartFileFromWhereItStopped() {
        val part = partial()
        part.writeBytes(data.copyOfRange(0, 100_000))
        FileDownloader.fetch(client(), "$base/file", data.size.toLong(), sha, part, { _, _ -> }, { true })
        assertEquals(sha, FileDownloader.sha256Of(part))
    }

    @Test
    fun stoppingKeepsWhatWasFetchedAndTheNextRunFinishes() {
        val part = partial()
        var chunks = 0
        try {
            FileDownloader.fetch(client(), "$base/file", data.size.toLong(), sha, part, { _, _ -> }, { chunks++ < 3 })
            fail("A stopped download must not report success.")
        } catch (expected: DownloadStopped) {
            assertTrue(expected.message.orEmpty().contains("resume"))
        }
        val kept = part.length()
        assertTrue("Some bytes should be kept, got $kept.", kept > 0 && kept < data.size)

        FileDownloader.fetch(client(), "$base/file", data.size.toLong(), sha, part, { _, _ -> }, { true })
        assertEquals(sha, FileDownloader.sha256Of(part))
    }

    @Test
    fun aChecksumMismatchIsThrownAway() {
        val part = partial()
        try {
            FileDownloader.fetch(client(), "$base/file", data.size.toLong(), "0".repeat(64), part, { _, _ -> }, { true })
            fail("A file that fails its checksum must be refused.")
        } catch (expected: IOException) {
            assertTrue(expected.message.orEmpty().contains("checksum"))
        }
        assertFalse(part.exists())
    }

    @Test
    fun followsARedirectAndSendsTheTokenToTheSameHost() {
        val part = partial()
        FileDownloader.fetch(client("tok"), "$base/redirect", data.size.toLong(), sha, part, { _, _ -> }, { true })
        assertEquals(sha, FileDownloader.sha256Of(part))
        assertTrue(seenAuthorization.isNotEmpty())
        assertTrue(seenAuthorization.all { it == "Bearer tok" })
    }

    @Test
    fun aGatedFileNeedsATokenAndSaysSo() {
        try {
            FileDownloader.fetch(client(), "$base/gated", 10, null, partial("gated.part"), { _, _ -> }, { true })
            fail("A refused file must not download.")
        } catch (expected: HfException) {
            assertTrue(expected.message.orEmpty().contains("token"))
        }
    }

    @Test
    fun repoListingIsReadFromTheApi() {
        val listing = client().repoFiles(HfSpec("org/repo", null))
        assertEquals("abc123", listing.revision)
        assertEquals("LFM2-2.6B-Q4_K_M.gguf", HuggingFaceFiles.choose(listing.files, null).name)
    }
}
