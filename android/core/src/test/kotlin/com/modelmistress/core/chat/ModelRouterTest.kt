package com.modelmistress.core.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class ModelRouterTest {
    @Test
    fun missingAndAutoUseTheDefaultModel() {
        val router = ModelRouter(FakeCatalog(setOf("tiny", "big"), default = "big"))
        assertEquals("big", router.resolve(null).modelId)
        assertEquals("big", router.resolve("  ").modelId)
        assertEquals("big", router.resolve("auto").modelId)
        assertEquals("tiny", router.resolve("tiny").modelId)
    }

    @Test
    fun fallsBackToTheFirstModelWhenTheDefaultIsGone() {
        val router = ModelRouter(FakeCatalog(setOf("tiny"), default = "deleted"))
        assertEquals("tiny", router.resolve(null).modelId)
    }

    @Test
    fun unknownModelsAndEmptyCatalogsAreRefused() {
        val router = ModelRouter(FakeCatalog(setOf("tiny"), default = "tiny"))
        val missing = assertThrows(ApiException::class.java) { router.resolve("missing") }
        assertEquals(404, missing.status)
        val empty = ModelRouter(FakeCatalog(emptySet(), default = null))
        val none = assertThrows(ApiException::class.java) { empty.resolve(null) }
        assertEquals(503, none.status)
    }

    private class FakeBackend : ChatBackend {
        override fun complete(
            messages: List<ChatMessage>,
            options: GenerationOptions,
            sink: TokenSink,
        ): GenerationStats {
            sink.onText("ok")
            return GenerationStats(1, 1, FinishReason.STOP)
        }
    }

    private class FakeCatalog(private val ids: Set<String>, private val default: String?) : ModelCatalog {
        override fun models(): List<ModelInfo> = ids.map { ModelInfo(it, null, 0L, it) }
        override fun defaultModelId(): String? = default
        override fun backend(modelId: String): ChatBackend? = if (modelId in ids) FakeBackend() else null
    }
}
