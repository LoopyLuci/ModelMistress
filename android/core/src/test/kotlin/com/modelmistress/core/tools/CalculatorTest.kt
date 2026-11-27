package com.modelmistress.core.tools

import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.Test

class CalculatorTest {
    @Test
    fun followsPrecedenceAndParentheses() {
        assertEquals(14.0, Calculator.evaluate("2+3*4"), 1e-9)
        assertEquals(20.0, Calculator.evaluate("(2+3)*4"), 1e-9)
        assertEquals(391.0, Calculator.evaluate("17*23"), 1e-9)
        assertEquals(2.5, Calculator.evaluate("10/4"), 1e-9)
    }

    @Test
    fun powerIsRightAssociativeAndMinusAppliesAfterIt() {
        assertEquals(512.0, Calculator.evaluate("2^3^2"), 1e-9)
        assertEquals(-4.0, Calculator.evaluate("-2^2"), 1e-9)
        assertEquals(4.0, Calculator.evaluate("(-2)^2"), 1e-9)
    }

    @Test
    fun knowsSqrtAndConstants() {
        assertEquals(4.0, Calculator.evaluate("sqrt(16)"), 1e-9)
        assertEquals(Math.PI, Calculator.evaluate("pi"), 1e-12)
        assertEquals(Math.E, Calculator.evaluate("E"), 1e-12)
    }

    @Test
    fun refusesBadInput() {
        expectRefusal("1/0", "division by zero")
        expectRefusal("sqrt(-1)", "negative")
        expectRefusal("2+", "unexpected character")
        expectRefusal("2 3", "unexpected character")
        expectRefusal("foo(1)", "unknown word")
        expectRefusal("(2+3", "missing )")
    }

    private fun expectRefusal(expression: String, reason: String) {
        try {
            Calculator.evaluate(expression)
            fail("$expression should be refused")
        } catch (error: IllegalArgumentException) {
            assertEquals(true, error.message.orEmpty().contains(reason))
        }
    }
}
