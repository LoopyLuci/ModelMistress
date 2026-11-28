package com.modelmistress.server

import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.chat.ModelInfo
import com.modelmistress.core.chat.ModelRouter
import com.modelmistress.core.chat.TokenSink
import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.net.Socket
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** A model file that another ModelMistress sends: the bytes are streamed to the store, and the key is checked first. */
class UploadTest {
    private val key = "DEVICE-KEY"

    /** Records what arrived; reads the whole body, so a short or long read shows up as a wrong count. */
    private class Store : ModelUploads {
        var name: String? = null
        var received: ByteArray? = null
        var declared: Long = -1

        override fun receive(name: String, body: InputStream, bytes: Long): String {
            this.name = name
            declared = bytes
            // reads exactly the declared length, as the phone's store does: the client keeps the socket open for the reply,
            // so reading to end-of-stream would wait until the read timeout
            val out = ByteArrayOutputStream()
            val buffer = ByteArray(4096)
            var left = bytes
            while (left > 0) {
                val n = body.read(buffer, 0, minOf(buffer.size.toLong(), left).toInt())
                if (n < 0) break
                out.write(buffer, 0, n)
                left -= n
            }
            received = out.toByteArray()
            return name.removeSuffix(".gguf")
        }
    }

    private val store = Store()
    private val catalog = object : ModelCatalog {
        override fun models(): List<ModelInfo> = emptyList()
        override fun defaultModelId(): String? = null
        override fun backend(modelId: String): ChatBackend? = null
    }
    private lateinit var server: ModelMistressServer

    @Before
    fun start() {
        server = ModelMistressServer(
            ModelRouter(catalog),
            ServerConfig(port = 0, apiKey = key),
            uploads = store,
        )
        server.start()
    }

    @After
    fun stop() {
        server.close()
    }

    /** Sends a raw POST with the given head and body, and returns the status line and the body of the reply. */
    private fun post(path: String, auth: String?, body: ByteArray, declaredLength: Long = body.size.toLong()): Pair<Int, String> {
        Socket("127.0.0.1", server.port).use { socket ->
            val head = StringBuilder()
            head.append("POST $path HTTP/1.1\r\nHost: 127.0.0.1\r\n")
            if (auth != null) head.append("Authorization: $auth\r\n")
            head.append("Content-Type: application/octet-stream\r\n")
            head.append("Content-Length: $declaredLength\r\nConnection: close\r\n\r\n")
            socket.getOutputStream().apply {
                write(head.toString().toByteArray(Charsets.US_ASCII))
                write(body)
                flush()
            }
            val reply = socket.getInputStream().readBytes().toString(Charsets.UTF_8)
            val status = reply.lineSequence().first().split(' ')[1].toInt()
            return status to reply.substringAfter("\r\n\r\n")
        }
    }

    @Test
    fun aModelIsStreamedIntoTheStoreWithItsNameAndLength() {
        // more than one read buffer, so the body is really streamed in parts
        val model = ByteArray(300_000) { (it % 251).toByte() }
        val (status, reply) = post("$UPLOAD_PATH?name=Tiny-Q4_K_M.gguf", "Bearer $key", model)
        assertEquals(200, status)
        assertTrue(reply, reply.contains("\"id\""))
        assertEquals("Tiny-Q4_K_M.gguf", store.name)
        assertEquals(model.size.toLong(), store.declared)
        assertTrue("every byte arrives in order", model.contentEquals(store.received))
    }

    @Test
    fun aWrongOrMissingKeyIsRefusedBeforeAnyByteIsStored() {
        val (status, reply) = post("$UPLOAD_PATH?name=Tiny.gguf", "Bearer nope", ByteArray(10))
        assertEquals(401, status)
        assertTrue(reply, reply.contains("invalid_api_key"))
        val (none, _) = post("$UPLOAD_PATH?name=Tiny.gguf", null, ByteArray(10))
        assertEquals(401, none)
        assertNull("nothing was stored", store.received)
    }

    @Test
    fun aSendWithoutALengthIsRefused() {
        Socket("127.0.0.1", server.port).use { socket ->
            socket.getOutputStream().apply {
                write("POST $UPLOAD_PATH?name=Tiny.gguf HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer $key\r\nConnection: close\r\n\r\n".toByteArray())
                flush()
            }
            val reply = socket.getInputStream().readBytes().toString(Charsets.UTF_8)
            assertTrue(reply, reply.lineSequence().first().contains("411"))
        }
    }

    @Test
    fun theRouteIsNotServedWithoutAStore() {
        val plain = ModelMistressServer(ModelRouter(catalog), ServerConfig(port = 0, apiKey = key))
        plain.start()
        try {
            Socket("127.0.0.1", plain.port).use { socket ->
                socket.getOutputStream().apply {
                    write("POST $UPLOAD_PATH?name=Tiny.gguf HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer $key\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".toByteArray())
                    flush()
                }
                val reply = socket.getInputStream().readBytes().toString(Charsets.UTF_8)
                assertTrue(reply, reply.lineSequence().first().contains("404"))
            }
        } finally {
            plain.close()
        }
    }
}
