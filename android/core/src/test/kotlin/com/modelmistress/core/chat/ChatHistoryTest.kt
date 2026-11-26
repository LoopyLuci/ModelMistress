package com.modelmistress.core.chat

import com.modelmistress.core.json.JsonException
import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class ChatHistoryTest {
    private fun chat(id: String, updatedAt: Long, text: String = "Hello") = SavedChat(
        id = id,
        title = SavedChat.titleFor(listOf(ChatMessage(Role.USER, text))),
        modelId = "LFM2-2.6B-Q4_0",
        createdAt = 1_000L,
        updatedAt = updatedAt,
        messages = listOf(ChatMessage(Role.USER, text), ChatMessage(Role.ASSISTANT, "Hi \"there\"\nnew line é")),
    )

    @Test
    fun roundTripKeepsEveryField() {
        val original = chat("abc-1", updatedAt = 2_000L, text = "Quotes \" and \\ and ünïcode")
        val decoded = ChatCodec.decode(ChatCodec.encode(original))
        assertEquals(original, decoded)
    }

    @Test
    fun roundTripKeepsANullModel() {
        val original = chat("abc-2", updatedAt = 2_000L).copy(modelId = null)
        assertNull(ChatCodec.decode(ChatCodec.encode(original)).modelId)
    }

    @Test
    fun decodeRefusesOtherVersions() {
        val text = ChatCodec.encode(chat("abc-3", 1L)).replace("\"version\":1", "\"version\":2")
        expectRefused(text)
    }

    @Test
    fun decodeRefusesUnknownRoles() {
        val text = ChatCodec.encode(chat("abc-4", 1L)).replace("\"role\":\"user\"", "\"role\":\"robot\"")
        expectRefused(text)
    }

    @Test
    fun decodeRefusesIdsThatAreNotFileSafe() {
        val text = ChatCodec.encode(chat("abc-5", 1L)).replace("\"id\":\"abc-5\"", "\"id\":\"../escape\"")
        try {
            ChatCodec.decode(text)
            fail("An id with a path separator must be refused.")
        } catch (expected: IllegalArgumentException) {
            assertTrue(expected.message.orEmpty().isNotEmpty())
        }
    }

    @Test
    fun titleIsTheFirstLineOfTheFirstQuestion() {
        val messages = listOf(
            ChatMessage(Role.USER, "\n  What is a GGUF file?\nLonger second line"),
            ChatMessage(Role.ASSISTANT, "A model file."),
        )
        assertEquals("What is a GGUF file?", SavedChat.titleFor(messages))
    }

    @Test
    fun titleIsCutAndSaysNewChatWhenThereIsNoQuestion() {
        val long = "x".repeat(200)
        val title = SavedChat.titleFor(listOf(ChatMessage(Role.USER, long)))
        assertTrue(title.length <= SavedChat.MAX_TITLE)
        assertTrue(title.endsWith("…"))
        assertEquals("New chat", SavedChat.titleFor(listOf(ChatMessage(Role.ASSISTANT, "Hello"))))
    }

    @Test
    fun storeListsNewestFirstAndSkipsFilesThatAreBroken() {
        val folder = Files.createTempDirectory("chats").toFile()
        try {
            val store = FileChatStore(folder)
            store.save(chat("older", updatedAt = 10L))
            store.save(chat("newer", updatedAt = 20L))
            File(folder, "broken.json").writeText("{ not json")
            File(folder, "renamed.json").writeText(ChatCodec.encode(chat("other", 30L)))

            assertEquals(listOf("newer", "older"), store.list().map { it.id })
            assertEquals(2, store.list().first().messageCount)
        } finally {
            folder.deleteRecursively()
        }
    }

    @Test
    fun storeLoadsReplacesAndDeletes() {
        val folder = Files.createTempDirectory("chats").toFile()
        try {
            val store = FileChatStore(folder)
            store.save(chat("one", updatedAt = 10L, text = "first"))
            store.save(chat("one", updatedAt = 11L, text = "second"))
            assertEquals("second", store.load("one")?.messages?.first()?.content)
            assertEquals(1, store.list().size)

            assertTrue(store.delete("one"))
            assertFalse(store.delete("one"))
            assertNull(store.load("one"))
            assertNull(store.load("../one"))
        } finally {
            folder.deleteRecursively()
        }
    }

    @Test
    fun storeKeepsOnlyTheNewestChatsWithinItsLimit() {
        val folder = Files.createTempDirectory("chats").toFile()
        try {
            val store = FileChatStore(folder, limit = 2)
            store.save(chat("a", updatedAt = 1L))
            store.save(chat("b", updatedAt = 2L))
            store.save(chat("c", updatedAt = 3L))
            assertEquals(listOf("c", "b"), store.list().map { it.id })
            assertNull(store.load("a"))
        } finally {
            folder.deleteRecursively()
        }
    }

    private fun expectRefused(text: String) {
        try {
            ChatCodec.decode(text)
            fail("The saved chat should have been refused.")
        } catch (expected: JsonException) {
            assertTrue(expected.message.orEmpty().isNotEmpty())
        }
    }
}
