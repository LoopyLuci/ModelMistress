package com.modelmistress.core.tools

/**
 * How much the model may use tools, set by the owner.
 *
 * - [OFF]: no tool is offered; the chat is plain.
 * - [ON]: reading tools (the clock, the calculator, files to read and search, the device status) run at once. A change to a
 *   file asks the owner first.
 * - [FULL_AUTO]: every tool runs at once, file changes included, as long as [FileWrites] allows them.
 */
enum class ToolUse { OFF, ON, FULL_AUTO }

/**
 * Whether the model may change files in the sandbox, set by the owner.
 *
 * - [OFF]: no file is written or edited, and the write tools are not offered.
 * - [ON]: each write or edit asks the owner first.
 * - [FULL_AUTO]: writes and edits run at once, but only while [ToolUse] is [ToolUse.FULL_AUTO]. Under [ToolUse.ON] they still ask.
 */
enum class FileWrites { OFF, ON, FULL_AUTO }

/** Turns the two owner settings into the policy the tool loop applies to each class of tool. */
object ToolModes {
    /** The policy for every class of tool under these two settings. Only reading and writing are ever allowed. */
    fun policies(toolUse: ToolUse, fileWrites: FileWrites): ToolPolicies = ToolPolicies(
        mapOf(
            ToolClass.READ to if (toolUse == ToolUse.OFF) ToolPolicy.DENY else ToolPolicy.AUTO,
            ToolClass.WRITE to writePolicy(toolUse, fileWrites),
            ToolClass.EXEC to ToolPolicy.DENY,
            ToolClass.NET to ToolPolicy.DENY,
            ToolClass.HUB to ToolPolicy.DENY,
        ),
    )

    /** How a file change is handled: refused, asked for, or run. The stricter of the two settings wins. */
    fun writePolicy(toolUse: ToolUse, fileWrites: FileWrites): ToolPolicy = when {
        toolUse == ToolUse.OFF || fileWrites == FileWrites.OFF -> ToolPolicy.DENY
        toolUse == ToolUse.ON || fileWrites == FileWrites.ON -> ToolPolicy.ASK
        else -> ToolPolicy.AUTO
    }

    /** True when the write and edit tools are offered at all. They are left out of the list the model sees otherwise. */
    fun writesOffered(toolUse: ToolUse, fileWrites: FileWrites): Boolean =
        writePolicy(toolUse, fileWrites) != ToolPolicy.DENY
}
