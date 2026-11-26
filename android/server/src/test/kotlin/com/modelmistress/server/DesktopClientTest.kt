package com.modelmistress.server

import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.chat.ModelInfo
import com.modelmistress.core.chat.ModelRouter
import com.modelmistress.core.chat.Role
import com.modelmistress.core.remote.CombinedCatalog
import com.modelmistress.core.remote.DesktopClient
import com.modelmistress.core.remote.DesktopHub
import com.modelmistress.core.remote.OverlayEndpoints
import java.io.IOException
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The phone side of a paired desktop, run against a real ModelMistress server on loopback: the same OpenAI routes the
 * desktop hub serves, with a peer key checked on every call.
 */
class DesktopClientTest {
    private val echo = object : ChatBackend {
        override fun complete(
            messages: List<ChatMessage>,
            options: GenerationOptions,
            sink: com.modelmistress.core.chat.TokenSink,
        ): GenerationStats {
            for (piece in listOf("Echo: ", messages.last().content)) {
                if (!sink.onText(piece)) break
            }
            return GenerationStats(promptTokens = messages.size, completionTokens = 2, finishReason = FinishReason.STOP)
        }
    }

    private val desktopModels = object : ModelCatalog {
        override fun models(): List<ModelInfo> = listOf(ModelInfo("tiny", 4096, 1000L, "llama Q4_K_M"))
        override fun defaultModelId(): String? = "tiny"
        override fun backend(modelId: String): ChatBackend? = if (modelId == "tiny") echo else null
    }

    private val noLocalModels = object : ModelCatalog {
        override fun models(): List<ModelInfo> = emptyList()
        override fun defaultModelId(): String? = null
        override fun backend(modelId: String): ChatBackend? = null
    }

    private lateinit var server: ModelMistressServer
    private lateinit var hub: DesktopHub

    @Before
    fun start() {
        server = ModelMistressServer(ModelRouter(desktopModels), ServerConfig(port = 0, apiKey = KEY))
        val port = server.start()
        hub = DesktopHub.of("office", " http://127.0.0.1:$port/v1/ ", KEY)
    }

    @After
    fun stop() {
        server.close()
    }

    @Test
    fun hubAddressesAreNormalised() {
        assertEquals("http://100.101.98.77:8000", DesktopHub.of("home", "http://100.101.98.77:8000/v1/", KEY).url)
        assertEquals("https://desk.example.ts.net", DesktopHub.of("home", " https://desk.example.ts.net ", KEY).url)
        assertThrows(IllegalArgumentException::class.java) { DesktopHub.of("home", "100.101.98.77:8000", KEY) }
        assertThrows(IllegalArgumentException::class.java) { DesktopHub.of("bad@name", "http://a", KEY) }
    }

    @Test
    fun clientListsTheDesktopsModelsWithTheKey() {
        val models = DesktopClient(hub).models()
        assertEquals(listOf("tiny"), models.map { it.id })
        assertEquals(4096, models.single().contextTokens)
        DesktopClient(hub).health()
    }

    @Test
    fun wrongKeyIsRefusedWithAClearMessage() {
        val wrong = DesktopHub.of("office", hub.url, "not-the-key")
        val error = assertThrows(ApiException::class.java) { DesktopClient(wrong).models() }
        assertEquals("desktop_key_refused", error.code)
        assertEquals(502, error.status)
    }

    @Test
    fun clientStreamsAReplyFromTheDesktop() {
        val pieces = mutableListOf<String>()
        val stats = DesktopClient(hub).chat(
            model = "tiny",
            messages = listOf(ChatMessage(Role.USER, "hi")),
            options = GenerationOptions(),
        ) { piece ->
            pieces += piece
            true
        }
        assertEquals("Echo: hi", pieces.joinToString(""))
        assertEquals(FinishReason.STOP, stats.finishReason)
    }

    @Test
    fun stoppingTheSinkStopsTheReply() {
        val pieces = mutableListOf<String>()
        DesktopClient(hub).chat("tiny", listOf(ChatMessage(Role.USER, "hi")), GenerationOptions()) { piece ->
            pieces += piece
            false
        }
        assertEquals(listOf("Echo: "), pieces)
    }

    @Test
    fun unreachableDesktopIsReportedAsUnreachable() {
        // Port 1 on loopback has no listener, so the connection is refused at once.
        val gone = DesktopHub.of("gone", "http://127.0.0.1:1", KEY)
        val error = assertThrows(ApiException::class.java) { DesktopClient(gone).health() }
        assertEquals("desktop_unreachable", error.code)
        assertEquals(503, error.status)
    }

    @Test
    fun combinedCatalogOffersAndRoutesTheDesktopsModels() {
        val catalog = CombinedCatalog(noLocalModels, { listOf(hub) }, clock = { 0L })
        val models = catalog.models()
        assertEquals(listOf("office@tiny"), models.map { it.id })
        assertTrue(models.single().description.startsWith("On office"))

        val backend = assertNotNullBackend(catalog.backend("office@tiny"))
        val pieces = mutableListOf<String>()
        backend.complete(listOf(ChatMessage(Role.USER, "yo")), GenerationOptions(), com.modelmistress.core.chat.TokenSink {
            pieces += it
            true
        })
        assertEquals("Echo: yo", pieces.joinToString(""))

        assertNull(catalog.backend("nowhere@tiny"))
        assertNull(catalog.backend("tiny"))
    }

    @Test
    fun combinedCatalogSkipsADesktopThatIsDown() {
        val gone = DesktopHub.of("gone", "http://127.0.0.1:1", KEY)
        val catalog = CombinedCatalog(noLocalModels, { listOf(gone, hub) }, clock = { 0L })
        assertEquals(listOf("office@tiny"), catalog.models().map { it.id })
    }

    @Test
    fun overlayDesktopIsReachedAtTheEndpointTheOverlayGives() {
        // The overlay gives a loopback endpoint for a paired node; here it is the test server itself.
        val endpoints = OverlayEndpoints { id ->
            if (id == OVERLAY_ID) hub.url else throw IOException("no paired node $id")
        }
        val overlayHub = DesktopHub.overlay("home", OVERLAY_ID.uppercase(), KEY)
        assertEquals("overlay://$OVERLAY_ID", overlayHub.url)
        assertTrue(overlayHub.throughOverlay)

        assertEquals(listOf("tiny"), DesktopClient(overlayHub, endpoints).models().map { it.id })
        val pieces = mutableListOf<String>()
        DesktopClient(overlayHub, endpoints).chat("tiny", listOf(ChatMessage(Role.USER, "hi")), GenerationOptions()) {
            pieces += it
            true
        }
        assertEquals("Echo: hi", pieces.joinToString(""))
    }

    @Test
    fun overlayDesktopWithoutAnOverlayIsReportedAsUnreachable() {
        val overlayHub = DesktopHub.overlay("home", OVERLAY_ID, KEY)
        val error = assertThrows(ApiException::class.java) { DesktopClient(overlayHub).health() }
        assertEquals("desktop_unreachable", error.code)
        assertEquals(503, error.status)
    }

    @Test
    fun overlayIdIsAFullNodeIdAndNamesTheDesktop() {
        assertThrows(IllegalArgumentException::class.java) { DesktopHub.overlay("home", "abc123", KEY) }
        assertThrows(IllegalArgumentException::class.java) { DesktopHub("home", "http://a", KEY, overlayId = OVERLAY_ID) }
        assertThrows(IllegalArgumentException::class.java) { DesktopHub("home", "overlay://$OVERLAY_ID", "  ", OVERLAY_ID) }
        assertFalse(DesktopHub.of("home", "http://a", KEY).throughOverlay)
    }

    private fun assertNotNullBackend(backend: ChatBackend?): ChatBackend =
        backend ?: throw AssertionError("expected a backend for the desktop model")

    private companion object {
        const val KEY = "peer-key-for-tests"
        const val OVERLAY_ID = "5f0c1e2d9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d"
    }
}
