package com.modelmistress.core.tools

import java.io.File
import java.io.IOException
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone

/** A class of tool. Policy is set per class, so the owner decides once for each kind of effect. */
enum class ToolClass { READ, WRITE, EXEC, NET, HUB }

/** What a class of tool does when the model calls it: runs at once, waits for the owner, or is refused. */
enum class ToolPolicy { AUTO, ASK, DENY }

/** The owner's policy for each class. Reading runs at once; anything that changes things asks; the rest is off. */
class ToolPolicies(private val rules: Map<ToolClass, ToolPolicy> = emptyMap()) {
    fun of(toolClass: ToolClass): ToolPolicy = rules[toolClass] ?: DEFAULTS.getValue(toolClass)

    companion object {
        val DEFAULTS: Map<ToolClass, ToolPolicy> = mapOf(
            ToolClass.READ to ToolPolicy.AUTO,
            ToolClass.WRITE to ToolPolicy.ASK,
            ToolClass.EXEC to ToolPolicy.DENY,
            ToolClass.NET to ToolPolicy.DENY,
            ToolClass.HUB to ToolPolicy.ASK,
        )
    }
}

data class ToolParameter(val name: String, val description: String, val required: Boolean)

data class ToolSpec(
    val name: String,
    val toolClass: ToolClass,
    val description: String,
    val parameters: List<ToolParameter>,
)

/** A call the model asked for. Every argument is text; numbers are sent as their digits. */
data class ToolCall(val name: String, val arguments: Map<String, String>)

sealed interface ToolOutcome {
    /** The tool ran. [text] is the result the model reads. */
    data class Ok(val text: String) : ToolOutcome

    /** The call was not run: no such tool, the policy refused it, or an argument was missing. */
    data class Refused(val reason: String) : ToolOutcome

    /** The tool ran and could not finish, for example a missing file. */
    data class Failed(val reason: String) : ToolOutcome
}

/** The tools a loop can offer and run. [Toolbox] is the device's own; the Misty agent adds its tools on top. */
interface ToolSource {
    val specs: List<ToolSpec>

    fun spec(name: String): ToolSpec?

    /** Runs a call that the policy has already allowed. Never throws. */
    fun run(call: ToolCall): ToolOutcome
}

/**
 * The tools this device offers and how they run. Reading tools run at once. The file tools need a [workspace]; without
 * one, only the clock and calculator are offered. [writable] adds fs_write and fs_edit, which the policy decides for each call.
 */
class Toolbox(
    private val workspace: Workspace? = null,
    private val now: () -> Long = { System.currentTimeMillis() },
    private val zone: TimeZone = TimeZone.getDefault(),
    private val writable: Boolean = false,
) : ToolSource {
    override val specs: List<ToolSpec> = buildList {
        add(
            ToolSpec(
                name = "datetime",
                toolClass = ToolClass.READ,
                description = "The current local date, time, weekday and time zone of this device.",
                parameters = emptyList(),
            ),
        )
        add(
            ToolSpec(
                name = "calc",
                toolClass = ToolClass.READ,
                description = "Evaluate arithmetic with + - * / ^, parentheses, pi, e and sqrt(x). Exact for the operators.",
                parameters = listOf(ToolParameter("expression", "The arithmetic expression, for example 17*23.", true)),
            ),
        )
        if (workspace != null) {
            add(
                ToolSpec(
                    name = "fs_list",
                    toolClass = ToolClass.READ,
                    description = "List a folder in the workspace. Use \".\" for the top folder.",
                    parameters = listOf(ToolParameter("path", "Folder inside the workspace.", false)),
                ),
            )
            add(
                ToolSpec(
                    name = "fs_read",
                    toolClass = ToolClass.READ,
                    description = "Read a text file in the workspace, optionally only some lines.",
                    parameters = listOf(
                        ToolParameter("path", "File inside the workspace.", true),
                        ToolParameter("start_line", "First line, counting from 1.", false),
                        ToolParameter("end_line", "Last line, inclusive.", false),
                    ),
                )
            )
            add(
                ToolSpec(
                    name = "fs_search",
                    toolClass = ToolClass.READ,
                    description = "Find lines containing some text in the workspace files.",
                    parameters = listOf(
                        ToolParameter("query", "Text to find; case does not matter.", true),
                        ToolParameter("path", "Folder or file to search. Defaults to the whole workspace.", false),
                        ToolParameter("max_results", "Most matches to return, up to 100. Defaults to 20.", false),
                    ),
                )
            )
            if (writable) {
                add(
                    ToolSpec(
                        name = "fs_write",
                        toolClass = ToolClass.WRITE,
                        description = "Create or replace a text file in the workspace. The owner decides each write.",
                        parameters = listOf(
                            ToolParameter("path", "File inside the workspace, for example notes/todo.txt.", true),
                            ToolParameter("content", "The whole text to write to the file.", true),
                        ),
                    ),
                )
                add(
                    ToolSpec(
                        name = "fs_edit",
                        toolClass = ToolClass.WRITE,
                        description = "Change a text file in the workspace: replace one exact piece of text with new text. " +
                            "The old text must appear exactly once in the file. The owner decides each edit.",
                        parameters = listOf(
                            ToolParameter("path", "A file inside the workspace that already exists.", true),
                            ToolParameter("old_text", "The exact text to replace, including its spaces and line breaks.", true),
                            ToolParameter("new_text", "The text to put in its place. Leave it out to delete the old text.", false),
                        ),
                    ),
                )
            }
        }
    }

    override fun spec(name: String): ToolSpec? = specs.firstOrNull { it.name == name }

    /** Runs a call that the policy has already allowed. Never throws. */
    override fun run(call: ToolCall): ToolOutcome {
        val spec = spec(call.name) ?: return ToolOutcome.Refused("there is no tool called ${call.name}")
        val missing = spec.parameters.firstOrNull { it.required && call.arguments[it.name].isNullOrBlank() }
        if (missing != null) return ToolOutcome.Refused("missing argument ${missing.name}")
        return try {
            ToolOutcome.Ok(Bounded.cap(execute(call)))
        } catch (error: IllegalArgumentException) {
            ToolOutcome.Failed(error.message ?: "the tool failed")
        } catch (error: IOException) {
            ToolOutcome.Failed(error.message ?: "the tool could not read the file")
        }
    }

    private fun execute(call: ToolCall): String = when (call.name) {
        "datetime" -> datetime()
        "calc" -> formatNumber(Calculator.evaluate(call.arguments.getValue("expression")))
        "fs_list" -> list(call.arguments["path"].orEmpty())
        "fs_read" -> read(
            path = call.arguments.getValue("path"),
            start = integer(call, "start_line"),
            end = integer(call, "end_line"),
        )
        "fs_search" -> search(
            query = call.arguments.getValue("query"),
            path = call.arguments["path"].orEmpty(),
            max = integer(call, "max_results")?.coerceIn(1, MAX_SEARCH_RESULTS) ?: DEFAULT_SEARCH_RESULTS,
        )
        "fs_write" -> write(path = call.arguments.getValue("path"), content = call.arguments.getValue("content"))
        "fs_edit" -> edit(
            path = call.arguments.getValue("path"),
            oldText = call.arguments.getValue("old_text"),
            newText = call.arguments["new_text"].orEmpty(),
        )
        else -> throw IllegalArgumentException("there is no tool called ${call.name}")
    }

    private fun datetime(): String {
        val instant = now()
        val moment = Date(instant)
        fun format(pattern: String): String =
            SimpleDateFormat(pattern, Locale.US).apply { timeZone = zone }.format(moment)
        val offsetMinutes = zone.getOffset(instant) / 60_000
        val sign = if (offsetMinutes < 0) "-" else "+"
        val hours = kotlin.math.abs(offsetMinutes) / 60
        val minutes = kotlin.math.abs(offsetMinutes) % 60
        val offset = String.format(Locale.US, "%s%02d:%02d", sign, hours, minutes)
        return "Local time: ${format("yyyy-MM-dd HH:mm")}, ${format("EEEE")}. " +
            "Time zone: ${zone.id} (UTC offset $offset)."
    }

    private fun list(path: String): String {
        val ws = workspace ?: throw IllegalArgumentException("there is no workspace")
        val folder = ws.resolve(path)
        val shown = ws.relativePath(folder)
        if (!folder.isDirectory) throw IllegalArgumentException("$shown is not a folder")
        if (Workspace.isSecret(shown)) throw IllegalArgumentException("$shown is not listed")
        val entries = folder.listFiles().orEmpty()
            .filter { !Workspace.isSecret(ws.relativePath(it)) }
            .sortedBy { it.name.lowercase() }
            .take(MAX_LIST_ENTRIES)
        if (entries.isEmpty()) return "Folder $shown is empty."
        val lines = entries.map { entry ->
            if (entry.isDirectory) "${entry.name}/" else "${entry.name} (${entry.length()} bytes)"
        }
        return "Folder $shown:\n" + lines.joinToString("\n")
    }

    private fun read(path: String, start: Int?, end: Int?): String {
        val ws = workspace ?: throw IllegalArgumentException("there is no workspace")
        val file = ws.resolve(path)
        val shown = ws.relativePath(file)
        if (!file.isFile) throw IllegalArgumentException("$shown is not a file")
        if (Workspace.isSecret(shown)) throw IllegalArgumentException("$shown is a secret and is not read")
        if (file.length() > MAX_READ_BYTES) throw IllegalArgumentException("$shown is larger than 2 MB")
        val bytes = file.readBytes()
        if (bytes.contains(0)) throw IllegalArgumentException("$shown is a binary file")
        val lines = textLines(String(bytes, Charsets.UTF_8))
        val from = (start ?: 1).coerceAtLeast(1)
        val to = (end ?: lines.size).coerceAtMost(lines.size)
        if (from > to) return "$shown has ${lines.size} lines; there are none in that range."
        return "$shown, lines $from-$to of ${lines.size}:\n" + lines.subList(from - 1, to).joinToString("\n")
    }

    private fun search(query: String, path: String, max: Int): String {
        val ws = workspace ?: throw IllegalArgumentException("there is no workspace")
        val base = ws.resolve(path)
        val needle = query.lowercase()
        val files = mutableListOf<File>()
        collect(ws, base, files, depth = 0)
        val matches = mutableListOf<String>()
        for (file in files) {
            if (matches.size >= max) break
            if (file.length() > MAX_READ_BYTES) continue
            val bytes = file.readBytes()
            if (bytes.contains(0)) continue
            val shown = ws.relativePath(file)
            textLines(String(bytes, Charsets.UTF_8)).forEachIndexed { index, line ->
                if (matches.size < max && line.lowercase().contains(needle)) {
                    matches += "$shown:${index + 1}: ${line.trim().take(MAX_LINE_CHARS)}"
                }
            }
        }
        if (matches.isEmpty()) return "No lines contain \"$query\"."
        val count = if (matches.size == 1) "1 match" else "${matches.size} matches"
        return "$count for \"$query\":\n" + matches.joinToString("\n")
    }

    private fun collect(ws: Workspace, entry: File, out: MutableList<File>, depth: Int) {
        if (out.size >= MAX_SEARCH_FILES || depth > MAX_SEARCH_DEPTH) return
        val shown = ws.relativePath(entry)
        if (Workspace.isSecret(shown)) return
        if (entry.isFile) {
            out += entry
        } else if (entry.isDirectory) {
            entry.listFiles().orEmpty().sortedBy { it.name.lowercase() }.forEach { collect(ws, it, out, depth + 1) }
        }
    }

    private fun write(path: String, content: String): String {
        val ws = workspace ?: throw IllegalArgumentException("there is no workspace")
        val existed = ws.resolve(path).isFile
        val shown = ws.write(path, content.toByteArray(Charsets.UTF_8))
        val verb = if (existed) "Replaced" else "Created"
        return "$verb $shown (${content.length} characters)."
    }

    /**
     * Replaces the one place where [oldText] appears in an existing file. Files with Windows line endings keep them. The
     * edit is refused when the text is missing or appears more than once, so the owner sees exactly one change.
     */
    private fun edit(path: String, oldText: String, newText: String): String {
        val ws = workspace ?: throw IllegalArgumentException("there is no workspace")
        val file = ws.resolve(path)
        val shown = ws.relativePath(file)
        if (!file.isFile) throw IllegalArgumentException("$shown is not a file; fs_write creates a new file")
        if (Workspace.isSecret(shown)) throw IllegalArgumentException("$shown is a secret and is not edited")
        if (file.length() > MAX_READ_BYTES) throw IllegalArgumentException("$shown is larger than 2 MB")
        val bytes = file.readBytes()
        if (bytes.contains(0)) throw IllegalArgumentException("$shown is a binary file")
        val text = String(bytes, Charsets.UTF_8)
        val windowsLines = text.contains("\r\n")
        val lf = text.replace("\r\n", "\n")
        val old = oldText.replace("\r\n", "\n")
        val count = occurrences(lf, old)
        if (count == 0) throw IllegalArgumentException("the old text was not found in $shown")
        if (count > 1) {
            throw IllegalArgumentException("the old text appears $count times in $shown; give more of it so it appears once")
        }
        val replaced = lf.replaceFirst(old, newText.replace("\r\n", "\n"))
        val updated = if (windowsLines) replaced.replace("\n", "\r\n") else replaced
        ws.write(shown, updated.toByteArray(Charsets.UTF_8))
        return "Edited $shown: one place changed."
    }

    private fun occurrences(text: String, part: String): Int {
        var count = 0
        var at = text.indexOf(part)
        while (at >= 0) {
            count++
            at = text.indexOf(part, at + part.length)
        }
        return count
    }

    private fun integer(call: ToolCall, name: String): Int? {
        val text = call.arguments[name]?.takeIf { it.isNotBlank() } ?: return null
        return text.trim().toIntOrNull() ?: throw IllegalArgumentException("$name must be a whole number")
    }

    private fun formatNumber(value: Double): String =
        if (value % 1.0 == 0.0 && kotlin.math.abs(value) < 1e15) value.toLong().toString() else value.toString()

    private companion object {
        const val MAX_LIST_ENTRIES = 300
        const val MAX_READ_BYTES = 2_000_000L
        const val MAX_LINE_CHARS = 300
        const val MAX_SEARCH_FILES = 2_000
        const val MAX_SEARCH_DEPTH = 6
        const val DEFAULT_SEARCH_RESULTS = 20
        const val MAX_SEARCH_RESULTS = 100
    }
}

/**
 * The tools of [base] that [names] allow, and no others. A call to any other tool is refused, as if the tool were not
 * there. Used to offer a question only the tools it needs.
 */
class ScopedToolSource(private val base: ToolSource, names: Collection<String>) : ToolSource {
    private val allowed: Set<String> = names.toSet()
    override val specs: List<ToolSpec> = base.specs.filter { it.name in allowed }

    override fun spec(name: String): ToolSpec? = specs.firstOrNull { it.name == name }

    override fun run(call: ToolCall): ToolOutcome =
        if (spec(call.name) == null) {
            ToolOutcome.Refused("there is no tool called ${call.name} for this question")
        } else {
            base.run(call)
        }
}

/** Keeps tool output short enough to fit the model's context. */
object Bounded {
    const val MAX_OUTPUT_CHARS = 4000

    fun cap(text: String): String {
        if (text.length <= MAX_OUTPUT_CHARS) return text
        return text.take(MAX_OUTPUT_CHARS) + "\n[${text.length - MAX_OUTPUT_CHARS} more characters cut]"
    }
}

/** A file's text as lines, without the newline at the end of the file. */
internal fun textLines(text: String): List<String> {
    val lines = text.replace("\r\n", "\n").split('\n')
    return if (lines.isNotEmpty() && lines.last().isEmpty()) lines.dropLast(1) else lines
}
