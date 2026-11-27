package com.modelmistress.core.tools

/**
 * Evaluates arithmetic the way the desktop `calc` tool does: `+ - * / ^`, parentheses, `pi`, `e` and `sqrt(...)`.
 * Unlike the desktop parser, a minus sign applies after a power, so `-2^2` is -4.
 */
object Calculator {
    fun evaluate(expression: String): Double {
        val parser = Parser(expression)
        val value = parser.expression()
        parser.skipSpaces()
        if (!parser.atEnd()) throw IllegalArgumentException("unexpected character at ${parser.position + 1}")
        if (!value.isFinite()) throw IllegalArgumentException("the result is not a finite number")
        return value
    }

    private class Parser(private val text: String) {
        var position = 0
            private set

        fun atEnd(): Boolean = position >= text.length

        fun skipSpaces() {
            while (position < text.length && text[position].isWhitespace()) position++
        }

        private fun eat(symbol: Char): Boolean {
            skipSpaces()
            if (position < text.length && text[position] == symbol) {
                position++
                return true
            }
            return false
        }

        fun expression(): Double {
            var value = term()
            while (true) {
                value = when {
                    eat('+') -> value + term()
                    eat('-') -> value - term()
                    else -> return value
                }
            }
        }

        private fun term(): Double {
            var value = unary()
            while (true) {
                value = when {
                    eat('*') -> value * unary()
                    eat('/') -> {
                        val divisor = unary()
                        if (divisor == 0.0) throw IllegalArgumentException("division by zero")
                        value / divisor
                    }
                    else -> return value
                }
            }
        }

        private fun unary(): Double {
            if (eat('-')) return -unary()
            if (eat('+')) return unary()
            return power()
        }

        private fun power(): Double {
            val base = atom()
            return if (eat('^')) Math.pow(base, unary()) else base
        }

        private fun atom(): Double {
            skipSpaces()
            if (eat('(')) {
                val value = expression()
                if (!eat(')')) throw IllegalArgumentException("missing )")
                return value
            }
            val start = position
            if (position < text.length && (text[position].isDigit() || text[position] == '.')) {
                while (position < text.length && (text[position].isDigit() || text[position] == '.')) position++
                return text.substring(start, position).toDoubleOrNull()
                    ?: throw IllegalArgumentException("\"${text.substring(start, position)}\" is not a number")
            }
            while (position < text.length && text[position].isLetter()) position++
            return when (val word = text.substring(start, position).lowercase()) {
                "pi" -> Math.PI
                "e" -> Math.E
                "sqrt" -> {
                    if (!eat('(')) throw IllegalArgumentException("sqrt needs (")
                    val value = expression()
                    if (!eat(')')) throw IllegalArgumentException("missing )")
                    if (value < 0.0) throw IllegalArgumentException("sqrt of a negative number")
                    Math.sqrt(value)
                }
                "" -> throw IllegalArgumentException("unexpected character at ${position + 1}")
                else -> throw IllegalArgumentException("unknown word \"$word\"")
            }
        }
    }
}
