package com.modelmistress.core.json

/** Thrown when text is not the JSON it should be. */
class JsonException(message: String) : IllegalArgumentException(message)

/**
 * A small JSON reader and writer for the API. Objects read as LinkedHashMap, arrays as List, whole numbers as Long and
 * other numbers as Double. Nesting is limited so a hostile body cannot exhaust the stack.
 */
object Json {
    private const val MAX_DEPTH = 64

    fun parse(text: String): Any? {
        val reader = Reader(text)
        val value = reader.value(0)
        reader.skipWhitespace()
        if (!reader.atEnd()) throw JsonException("Unexpected text after the JSON value at ${reader.position}.")
        return value
    }

    fun write(value: Any?): String {
        val out = StringBuilder()
        writeValue(out, value)
        return out.toString()
    }

    private fun writeValue(out: StringBuilder, value: Any?) {
        when (value) {
            null -> out.append("null")
            is String -> writeString(out, value)
            is Boolean -> out.append(if (value) "true" else "false")
            is Int, is Long -> out.append(value.toString())
            is Float -> writeValue(out, value.toDouble())
            is Double -> {
                if (!value.isFinite()) throw JsonException("JSON cannot hold $value.")
                out.append(value.toString())
            }
            is Map<*, *> -> {
                out.append('{')
                var first = true
                for ((key, item) in value) {
                    if (!first) out.append(',')
                    first = false
                    writeString(out, key as? String ?: throw JsonException("JSON object keys must be strings."))
                    out.append(':')
                    writeValue(out, item)
                }
                out.append('}')
            }
            is List<*> -> {
                out.append('[')
                value.forEachIndexed { index, item ->
                    if (index > 0) out.append(',')
                    writeValue(out, item)
                }
                out.append(']')
            }
            else -> throw JsonException("Cannot write ${value?.javaClass?.simpleName} as JSON.")
        }
    }

    private fun writeString(out: StringBuilder, text: String) {
        out.append('"')
        for (c in text) {
            when {
                c == '"' -> out.append("\\\"")
                c == '\\' -> out.append("\\\\")
                c == '\n' -> out.append("\\n")
                c == '\r' -> out.append("\\r")
                c == '\t' -> out.append("\\t")
                c == '\b' -> out.append("\\b")
                c == '\u000C' -> out.append("\\f")
                c < ' ' -> out.append("\\u").append(c.code.toString(16).padStart(4, '0'))
                else -> out.append(c)
            }
        }
        out.append('"')
    }

    private class Reader(private val text: String) {
        var position = 0
            private set

        fun atEnd(): Boolean = position >= text.length

        fun skipWhitespace() {
            while (position < text.length) {
                val c = text[position]
                if (c != ' ' && c != '\t' && c != '\n' && c != '\r') break
                position++
            }
        }

        fun value(depth: Int): Any? {
            if (depth > MAX_DEPTH) throw JsonException("The JSON nests too deeply.")
            skipWhitespace()
            return when (peek() ?: throw JsonException("The JSON ended early.")) {
                '{' -> obj(depth)
                '[' -> array(depth)
                '"' -> string()
                't' -> literal("true", true)
                'f' -> literal("false", false)
                'n' -> literal("null", null)
                else -> number()
            }
        }

        private fun obj(depth: Int): Map<String, Any?> {
            position++
            val result = LinkedHashMap<String, Any?>()
            skipWhitespace()
            if (peek() == '}') {
                position++
                return result
            }
            while (true) {
                skipWhitespace()
                if (peek() != '"') throw JsonException("JSON object keys must be strings.")
                val key = string()
                skipWhitespace()
                expect(':')
                result[key] = value(depth + 1)
                skipWhitespace()
                when (next()) {
                    ',' -> continue
                    '}' -> return result
                    else -> throw JsonException("Expected , or } in a JSON object.")
                }
            }
        }

        private fun array(depth: Int): List<Any?> {
            position++
            val result = ArrayList<Any?>()
            skipWhitespace()
            if (peek() == ']') {
                position++
                return result
            }
            while (true) {
                result.add(value(depth + 1))
                skipWhitespace()
                when (next()) {
                    ',' -> continue
                    ']' -> return result
                    else -> throw JsonException("Expected , or ] in a JSON array.")
                }
            }
        }

        private fun string(): String {
            expect('"')
            val out = StringBuilder()
            while (true) {
                val c = next()
                when {
                    c == '"' -> return out.toString()
                    c == '\\' -> escape(out)
                    c < ' ' -> throw JsonException("Control characters must be escaped in JSON strings.")
                    else -> out.append(c)
                }
            }
        }

        private fun escape(out: StringBuilder) {
            when (val e = next()) {
                '"', '\\', '/' -> out.append(e)
                'b' -> out.append('\b')
                'f' -> out.append('\u000C')
                'n' -> out.append('\n')
                'r' -> out.append('\r')
                't' -> out.append('\t')
                'u' -> {
                    if (position + 4 > text.length) throw JsonException("A \\u escape is cut short.")
                    val code = text.substring(position, position + 4).toIntOrNull(16)
                        ?: throw JsonException("A \\u escape is not hexadecimal.")
                    out.append(code.toChar())
                    position += 4
                }
                else -> throw JsonException("Unknown escape \\$e.")
            }
        }

        private fun number(): Any {
            val start = position
            if (peek() == '-') position++
            var digits = countDigits()
            var fractional = false
            if (peek() == '.') {
                fractional = true
                position++
                digits += countDigits()
            }
            if (digits == 0) throw JsonException("Expected a JSON value at $start.")
            if (peek() == 'e' || peek() == 'E') {
                fractional = true
                position++
                if (peek() == '+' || peek() == '-') position++
                if (countDigits() == 0) throw JsonException("A number's exponent is missing at $start.")
            }
            val token = text.substring(start, position)
            if (!fractional) {
                val whole = token.toLongOrNull()
                if (whole != null) return whole
            }
            return token.toDoubleOrNull() ?: throw JsonException("A number is malformed at $start.")
        }

        private fun countDigits(): Int {
            var count = 0
            while (position < text.length && text[position] in '0'..'9') {
                position++
                count++
            }
            return count
        }

        private fun literal(word: String, result: Any?): Any? {
            if (!text.startsWith(word, position)) throw JsonException("Unexpected text at $position.")
            position += word.length
            return result
        }

        private fun peek(): Char? = if (position < text.length) text[position] else null

        private fun next(): Char {
            if (position >= text.length) throw JsonException("The JSON ended early.")
            return text[position++]
        }

        private fun expect(expected: Char) {
            if (next() != expected) throw JsonException("Expected '$expected' at ${position - 1}.")
        }
    }
}
