package com.modelmistress.server

import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.nio.charset.StandardCharsets
import java.util.Locale

/** A request the HTTP layer refuses before it reaches the API. [status] is the status to answer with. */
class HttpException(val status: Int, message: String) : Exception(message)

class HttpRequest(
    val method: String,
    val path: String,
    val headers: Map<String, String>,
    val body: ByteArray,
    /** The part of the address after '?', as sent. Empty when there is none. */
    val query: String = "",
) {
    /** A header by name, matched without regard to case. */
    fun header(name: String): String? = headers[name.lowercase(Locale.ROOT)]
}

/** Reads one HTTP/1.1 request: the request line, headers, and a body sized by Content-Length or chunked encoding. */
object HttpReader {
    private const val MAX_HEAD_BYTES = 16 * 1024
    private const val MAX_LINE_BYTES = 1024
    private const val CRLF_CRLF = 0x0D0A0D0A

    fun read(input: InputStream, maxBodyBytes: Int): HttpRequest = withBody(readRequestHead(input), input, maxBodyBytes)

    /**
     * The request line and headers, read now. The body stays on [input]: a route that takes a body too big to hold
     * reads it itself (see [withBody]).
     */
    fun readRequestHead(input: InputStream): HttpRequest {
        val lines = readHead(input).split("\r\n")
        val requestLine = lines.first().split(' ')
        if (requestLine.size != 3 || !requestLine[2].startsWith("HTTP/1.")) {
            throw HttpException(400, "The request line is malformed.")
        }
        val headers = LinkedHashMap<String, String>()
        for (line in lines.drop(1)) {
            val colon = line.indexOf(':')
            if (colon <= 0) throw HttpException(400, "A header line is malformed.")
            headers[line.substring(0, colon).trim().lowercase(Locale.ROOT)] = line.substring(colon + 1).trim()
        }
        return HttpRequest(
            method = requestLine[0],
            path = requestLine[1].substringBefore('?'),
            headers = headers,
            body = ByteArray(0),
            query = requestLine[1].substringAfter('?', ""),
        )
    }

    /** The request with its body read from [input]: sized by Content-Length, or chunked, and at most [maxBodyBytes]. */
    fun withBody(head: HttpRequest, input: InputStream, maxBodyBytes: Int): HttpRequest {
        val body = if (head.header("transfer-encoding")?.lowercase(Locale.ROOT)?.contains("chunked") == true) {
            readChunked(input, maxBodyBytes)
        } else {
            val declared = head.header("content-length")
            val length = if (declared == null) {
                0
            } else {
                declared.toIntOrNull()?.takeIf { it >= 0 }
                    ?: throw HttpException(400, "Content-Length is not a number.")
            }
            if (length > maxBodyBytes) {
                throw HttpException(413, "The request body is larger than $maxBodyBytes bytes.")
            }
            readExactly(input, length)
        }
        return HttpRequest(head.method, head.path, head.headers, body, head.query)
    }

    private fun readHead(input: InputStream): String {
        val out = ByteArrayOutputStream()
        var tail = 0
        while (true) {
            val b = input.read()
            if (b < 0) throw HttpException(400, "The connection closed before the headers ended.")
            out.write(b)
            if (out.size() > MAX_HEAD_BYTES) throw HttpException(431, "The request headers are too large.")
            tail = (tail shl 8) or b
            if (tail == CRLF_CRLF) break
        }
        val text = String(out.toByteArray(), StandardCharsets.ISO_8859_1)
        return text.substring(0, text.length - 4)
    }

    private fun readChunked(input: InputStream, maxBodyBytes: Int): ByteArray {
        val out = ByteArrayOutputStream()
        while (true) {
            val size = readLine(input).substringBefore(';').trim().toIntOrNull(16)
                ?: throw HttpException(400, "A chunk size is malformed.")
            if (size < 0) throw HttpException(400, "A chunk size is malformed.")
            if (size == 0) {
                while (readLine(input).isNotEmpty()) {
                    // Trailers are read and ignored.
                }
                break
            }
            if (out.size() + size > maxBodyBytes) {
                throw HttpException(413, "The request body is larger than $maxBodyBytes bytes.")
            }
            out.write(readExactly(input, size))
            if (readLine(input).isNotEmpty()) throw HttpException(400, "A chunk is not followed by a line break.")
        }
        return out.toByteArray()
    }

    private fun readLine(input: InputStream): String {
        val out = ByteArrayOutputStream()
        while (true) {
            val b = input.read()
            if (b < 0) throw HttpException(400, "The request ended early.")
            if (b == '\n'.code) break
            out.write(b)
            if (out.size() > MAX_LINE_BYTES) throw HttpException(400, "A line is too long.")
        }
        return String(out.toByteArray(), StandardCharsets.ISO_8859_1).removeSuffix("\r")
    }

    private fun readExactly(input: InputStream, count: Int): ByteArray {
        val out = ByteArray(count)
        var read = 0
        while (read < count) {
            val n = input.read(out, read, count - read)
            if (n < 0) throw HttpException(400, "The request body ended early.")
            read += n
        }
        return out
    }
}

/** Writes one HTTP/1.1 response per connection. Every response closes the connection, so no framing is shared. */
class HttpResponseWriter(private val out: OutputStream) {
    /** True once the status line has gone out; after that only a stream's own events can follow. */
    var committed = false
        private set

    fun json(status: Int, body: String) {
        send(status, "application/json; charset=utf-8", body.toByteArray(StandardCharsets.UTF_8))
    }

    fun send(status: Int, contentType: String, body: ByteArray) {
        head(
            status,
            linkedMapOf(
                "Content-Type" to contentType,
                "Content-Length" to body.size.toString(),
                "Connection" to "close",
            ),
        )
        out.write(body)
        out.flush()
    }

    fun startEventStream() {
        head(
            200,
            linkedMapOf(
                "Content-Type" to "text/event-stream; charset=utf-8",
                "Cache-Control" to "no-cache",
                "Connection" to "close",
            ),
        )
        out.flush()
    }

    /** One server-sent event. Each call is flushed so the client sees the text as it is made. */
    fun event(data: String) {
        out.write("data: $data\n\n".toByteArray(StandardCharsets.UTF_8))
        out.flush()
    }

    private fun head(status: Int, headers: Map<String, String>) {
        check(!committed) { "The response has already started." }
        committed = true
        val text = StringBuilder("HTTP/1.1 ").append(status).append(' ').append(reasonPhrase(status)).append("\r\n")
        for ((name, value) in headers) {
            text.append(name).append(": ").append(value).append("\r\n")
        }
        text.append("\r\n")
        out.write(text.toString().toByteArray(StandardCharsets.UTF_8))
    }

    private fun reasonPhrase(status: Int): String = when (status) {
        200 -> "OK"
        400 -> "Bad Request"
        401 -> "Unauthorized"
        404 -> "Not Found"
        405 -> "Method Not Allowed"
        413 -> "Payload Too Large"
        431 -> "Request Header Fields Too Large"
        500 -> "Internal Server Error"
        503 -> "Service Unavailable"
        else -> "Response"
    }
}
