package com.modelmistress.core.agent

import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FileChatStore
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.Role
import com.modelmistress.core.chat.TokenSink
import com.modelmistress.core.environment.EnvironmentStore
import com.modelmistress.core.misty.FeedbackLabel
import com.modelmistress.core.misty.FileMistyFeedback
import com.modelmistress.core.tools.FileWrites
import com.modelmistress.core.tools.ToolUse
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class AgentServiceTest {
    @get:Rule
    val folder = TemporaryFolder()

    private val calcBlock = "<tool>{\"name\": \"calc\", \"arguments\": {\"expression\": \"17*23\"}}</tool>"

    private var toolUse = ToolUse.ON
    private var fileWrites = FileWrites.ON
    private var mistyEnabled = false

    private fun service(approvals: Approvals = Approvals(timeoutMillis = 200)): AgentService {
        val root = folder.root
        return AgentService(
            chats = FileChatStore(File(root, "chats").apply { mkdirs() }),
            environments = EnvironmentStore(root),
            feedback = FileMistyFeedback(File(root, "feedback.jsonl")),
            approvals = approvals,
            settings = { AgentSettings(toolUse, fileWrites, mistyEnabled) },
            reflex = { null },
            deviceStatus = { "Free memory: 1 GB" },
            installedModels = { "tiny" },
            now = { 1_000L },
        )
    }

    private fun request(
        backend: ChatBackend,
        text: String,
        chatId: String? = null,
        sandboxId: String? = null,
        misty: Boolean = false,
        toolsAllowed: Boolean = true,
    ) = AgentRequest(
        modelId = "tiny",
        model = backend,
        messages = listOf(ChatMessage(Role.USER, text)),
        options = GenerationOptions(),
        client = "MobileAgents",
        chatId = chatId,
        sandboxId = sandboxId,
        misty = misty,
        toolsAllowed = toolsAllowed,
    )

    @Test
    fun aPlainRequestGetsAReplyWithNoToolAndNoSavedChat() {
        val backend = Scripted("Hello there.")
        val service = service()
        val result = service.run(request(backend, "hi")) { true }

        assertEquals("Hello there.", result.answer)
        assertEquals(0, result.toolCalls)
        assertTrue(result.offered.isEmpty())
        assertFalse(backend.seen.single().any { it.role == Role.SYSTEM })
        assertNull(result.chatId)
        assertTrue(FileChatStore(File(folder.root, "chats")).list().isEmpty())
    }

    @Test
    fun streamsTheReplyAsTextPiecesThatJoinUpToTheAnswer() {
        val pieces = mutableListOf<String>()
        val result = service().run(request(Scripted("Hello there, friend."), "hi")) { event ->
            if (event is AgentEvent.Text) pieces += event.piece
            true
        }
        assertEquals("Hello there, friend.", pieces.joinToString(""))
        assertEquals("Hello there, friend.", result.answer)
    }

    @Test
    fun aChatRunsAToolKeepsItsSandboxAndSavesTheReply() {
        val backend = Scripted("Let me compute. $calcBlock", "It is 391.")
        val events = mutableListOf<AgentEvent>()
        val service = service()
        val result = service.run(request(backend, "calculate 17 times 23", chatId = "chat-1")) { event ->
            events += event
            true
        }

        assertEquals("It is 391.", result.answer)
        assertEquals(1, result.toolCalls)
        assertEquals(listOf("calc:ran"), result.calls)
        assertTrue("calc" in result.offered)
        assertTrue(events.contains(AgentEvent.ToolStarted("calc")))
        assertTrue(events.contains(AgentEvent.ToolFinished("calc", "ran")))
        assertFalse(events.filterIsInstance<AgentEvent.Text>().any { it.piece.contains("<tool>") })

        val saved = FileChatStore(File(folder.root, "chats")).load("chat-1")
        assertNotNull(saved)
        assertEquals(listOf(Role.USER, Role.ASSISTANT), saved!!.messages.map { it.role })
        assertEquals("It is 391.", saved.messages.last().content)
        val sandbox = saved.environmentId
        assertNotNull(sandbox)

        // The next turn of the same chat works in the same sandbox.
        service.run(request(Scripted("Noted."), "and again", chatId = "chat-1")) { true }
        assertEquals(sandbox, FileChatStore(File(folder.root, "chats")).load("chat-1")!!.environmentId)
    }

    @Test
    fun aWriteThatTheOwnerRefusesIsNotRunAndIsReportedAsRefused() {
        fileWrites = FileWrites.ON
        val writeBlock = "<tool>{\"name\": \"fs_write\", \"arguments\": {\"path\": \"notes.txt\", \"content\": \"hello\"}}</tool>"
        val backend = Scripted(writeBlock, "I could not save it.")
        val service = service(Approvals(timeoutMillis = 100))
        val result = service.run(request(backend, "create a file named notes.txt with hello", chatId = "chat-2")) { true }

        assertEquals(listOf("fs_write:refused"), result.calls)
        assertFalse(fileInSandboxOf("chat-2", "notes.txt").exists())
    }

    /** The file [name] in the sandbox that [chatId] works in. */
    private fun fileInSandboxOf(chatId: String, name: String): File {
        val sandbox = FileChatStore(File(folder.root, "chats")).load(chatId)!!.environmentId!!
        return EnvironmentStore(folder.root).folderOf(sandbox)!!.resolve(name)
    }

    @Test
    fun aWriteTheOwnerAllowsRunsAfterTheAnswer() {
        val approvals = Approvals(timeoutMillis = 5_000)
        val owner = Thread {
            val deadline = System.currentTimeMillis() + 5_000
            while (approvals.pending().isEmpty() && System.currentTimeMillis() < deadline) Thread.sleep(5)
            approvals.pending().forEach { approvals.answer(it.id, true) }
        }.apply { start() }
        val writeBlock = "<tool>{\"name\": \"fs_write\", \"arguments\": {\"path\": \"notes.txt\", \"content\": \"hello\"}}</tool>"
        val service = service(approvals)
        val result = service.run(
            request(Scripted(writeBlock, "Saved."), "create a file named notes.txt with hello", chatId = "chat-3"),
        ) { true }
        owner.join()

        assertEquals(listOf("fs_write:ran"), result.calls)
        assertEquals("hello", fileInSandboxOf("chat-3", "notes.txt").readText().trim())
    }

    @Test
    fun aRequestThatTurnsToolsOffGetsNoToolInstruction() {
        val backend = Scripted("No tools here.")
        val result = service().run(request(backend, "calculate 2 plus 2", chatId = "chat-4", toolsAllowed = false)) { true }
        assertEquals(0, result.toolCalls)
        assertFalse(backend.seen.single().any { it.role == Role.SYSTEM })
    }

    @Test
    fun mistyIsAskedForOnlyWhileTheOwnerHasHerOn() {
        mistyEnabled = false
        val error = assertThrows(ApiException::class.java) {
            service().run(request(Scripted("x"), "hi", misty = true)) { true }
        }
        assertEquals(409, error.status)
        assertEquals("misty_disabled", error.code)

        mistyEnabled = true
        val result = service().run(request(Scripted("Hi, I am Misty."), "hi", misty = true, chatId = "chat-5")) { true }
        assertTrue(result.misty)
    }

    @Test
    fun aMissingSandboxIsRefusedBeforeAnythingRuns() {
        val error = assertThrows(ApiException::class.java) {
            service().run(request(Scripted("x"), "hi", sandboxId = "no-such-sandbox")) { true }
        }
        assertEquals(404, error.status)
        assertEquals("sandbox_not_found", error.code)
    }

    @Test
    fun aChatIdThatCannotBeKeptIsRefused() {
        assertThrows(IllegalArgumentException::class.java) {
            service().run(request(Scripted("x"), "hi", chatId = "../escape")) { true }
        }
    }

    @Test
    fun aRatingGoesToTheLastReplyInTheChat() {
        val service = service()
        service.run(request(Scripted("Hello."), "hi", chatId = "chat-6")) { true }
        assertTrue(service.rate("chat-6", FeedbackLabel.GOOD))
        assertFalse(service.rate("no-such-chat", FeedbackLabel.GOOD))

        val examples = FileMistyFeedback(File(folder.root, "feedback.jsonl")).examples()
        assertEquals(1, examples.size)
        assertEquals(FeedbackLabel.GOOD, examples.single().label)
        assertEquals("hi", examples.single().question)
        assertEquals("Hello.", examples.single().answer)
    }

    @Test
    fun aCorrectionKeepsTheOwnersWords() {
        val service = service()
        service.run(request(Scripted("It is 5."), "what is two plus three", chatId = "chat-7")) { true }
        assertTrue(service.rate("chat-7", FeedbackLabel.CORRECTED, correction = "It is 5, not 6."))
        val example = FileMistyFeedback(File(folder.root, "feedback.jsonl")).examples().single()
        assertEquals("It is 5, not 6.", example.correction)
    }

    /** A model that replies with each script in turn and remembers what it was asked. */
    private class Scripted(vararg replies: String) : ChatBackend {
        private val replies = replies.toList()
        val seen = mutableListOf<List<ChatMessage>>()
        private var next = 0

        override fun complete(messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats {
            seen += messages.toList()
            val reply = replies.getOrElse(next) { "" }
            next++
            for (piece in reply.chunked(9)) {
                if (!sink.onText(piece)) break
            }
            return GenerationStats(0, 0, FinishReason.STOP)
        }
    }
}
