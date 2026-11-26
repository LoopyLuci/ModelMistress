package com.modelmistress.core.tools

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ToolModesTest {
    @Test
    fun defaultsKeepReadingAutomaticAndAskingForWrites() {
        val policies = ToolModes.policies(ToolUse.ON, FileWrites.ON)
        assertEquals(ToolPolicy.AUTO, policies.of(ToolClass.READ))
        assertEquals(ToolPolicy.ASK, policies.of(ToolClass.WRITE))
        assertEquals(ToolPolicy.DENY, policies.of(ToolClass.EXEC))
        assertEquals(ToolPolicy.DENY, policies.of(ToolClass.NET))
    }

    @Test
    fun toolUseOffRefusesEverything() {
        for (writes in FileWrites.entries) {
            val policies = ToolModes.policies(ToolUse.OFF, writes)
            assertEquals(ToolPolicy.DENY, policies.of(ToolClass.READ))
            assertEquals(ToolPolicy.DENY, policies.of(ToolClass.WRITE))
        }
    }

    @Test
    fun fileWritesOffRefusesWritesButKeepsReading() {
        for (use in listOf(ToolUse.ON, ToolUse.FULL_AUTO)) {
            val policies = ToolModes.policies(use, FileWrites.OFF)
            assertEquals(ToolPolicy.AUTO, policies.of(ToolClass.READ))
            assertEquals(ToolPolicy.DENY, policies.of(ToolClass.WRITE))
        }
    }

    @Test
    fun writesRunAtOnceOnlyWhenBothSettingsAreFullAuto() {
        assertEquals(ToolPolicy.AUTO, ToolModes.writePolicy(ToolUse.FULL_AUTO, FileWrites.FULL_AUTO))
        // Tool use On keeps asking for changes, even when file writes is full auto.
        assertEquals(ToolPolicy.ASK, ToolModes.writePolicy(ToolUse.ON, FileWrites.FULL_AUTO))
        // File writes On asks each time, even when tool use is full auto.
        assertEquals(ToolPolicy.ASK, ToolModes.writePolicy(ToolUse.FULL_AUTO, FileWrites.ON))
    }

    @Test
    fun writeToolsAreOfferedOnlyWhenTheirPolicyIsNotDeny() {
        assertFalse(ToolModes.writesOffered(ToolUse.OFF, FileWrites.FULL_AUTO))
        assertFalse(ToolModes.writesOffered(ToolUse.FULL_AUTO, FileWrites.OFF))
        assertTrue(ToolModes.writesOffered(ToolUse.ON, FileWrites.ON))
    }
}
