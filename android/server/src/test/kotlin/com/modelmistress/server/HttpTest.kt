package com.modelmistress.server

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

class HttpTest {
    @Test
    fun readsALengthDelimitedBodyAndDropsTheQuery() {
        val raw = "POST /v1/x?y=1 HTTP/1.1\r\nHost: a\r\nContent-Length: 5\r\n\r\nhello"
        val request = HttpReader.read(ByteArrayInputStream(raw.toByteArray()), maxBodyBytes = 100)
        assertEquals("POST", request.method)
        assertEquals("/v1/x", request.path)
        assertEquals("a", request.header("HOST"))
        assertEquals("hello", String(request.body))
    }

    @Test
    fun readsAChunkedBody() {
        val raw = "POST /v1/x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n4\r\n abc\r\n0\r\n\r\n"
        val request = HttpReader.read(ByteArrayInputStream(raw.toByteArray()), maxBodyBytes = 100)
        assertEquals("hello abc", String(request.body))
        assertNull(request.header("content-length"))
    }

    @Test
    fun refusesBodiesOverTheLimitAndMalformedRequests() {
        val big = "POST / HTTP/1.1\r\nContent-Length: 100\r\n\r\n"
        val tooBig = assertThrows(HttpException::class.java) {
            HttpReader.read(ByteArrayInputStream(big.toByteArray()), maxBodyBytes = 10)
        }
        assertEquals(413, tooBig.status)

        val malformed = assertThrows(HttpException::class.java) {
            HttpReader.read(ByteArrayInputStream("GET /\r\n\r\n".toByteArray()), maxBodyBytes = 10)
        }
        assertEquals(400, malformed.status)
    }

    @Test
    fun writesStatusHeadersAndEvents() {
        val out = ByteArrayOutputStream()
        val writer = HttpResponseWriter(out)
        writer.json(404, "{}")
        val text = out.toString(Charsets.UTF_8.name())
        assertEquals(true, text.startsWith("HTTP/1.1 404 Not Found\r\n"))
        assertEquals(true, text.contains("Content-Length: 2\r\n"))
        assertEquals(true, text.endsWith("\r\n\r\n{}"))
    }
}
