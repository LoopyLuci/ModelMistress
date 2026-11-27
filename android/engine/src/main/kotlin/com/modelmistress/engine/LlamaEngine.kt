package com.modelmistress.engine

import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.LocalEngine
import com.modelmistress.core.chat.TokenSink
import com.modelmistress.core.text.Utf8Chunker
import java.nio.charset.StandardCharsets
import kotlin.random.Random

/** One GGUF model loaded into llama.cpp on this device. Replies run one at a time. */
class LlamaEngine private constructor(
    private var handle: Long,
    val contextTokens: Int,
) : LocalEngine {

    @Synchronized
    override fun complete(messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats {
        check(handle != 0L) { "The model has been unloaded." }
        val roles = Array(messages.size) { messages[it].role.wire.toByteArray(StandardCharsets.UTF_8) }
        val contents = Array(messages.size) { messages[it].content.toByteArray(StandardCharsets.UTF_8) }
        val chunker = Utf8Chunker()
        var keepGoing = true
        val callback = TokenCallback { bytes ->
            val text = chunker.push(bytes)
            if (text.isNotEmpty()) keepGoing = sink.onText(text)
            keepGoing
        }
        val seed = options.seed ?: Random.nextLong(0, Int.MAX_VALUE.toLong())
        val stats = LlamaNative.nativeGenerate(
            handle,
            roles,
            contents,
            options.maxTokens,
            options.temperature,
            options.topP,
            options.topK,
            seed,
            callback,
        )
        val tail = chunker.finish()
        if (tail.isNotEmpty() && keepGoing) sink.onText(tail)
        return GenerationStats(
            promptTokens = stats[0],
            completionTokens = stats[1],
            finishReason = if (stats[2] == REASON_LENGTH) FinishReason.LENGTH else FinishReason.STOP,
        )
    }

    @Synchronized
    override fun close() {
        if (handle != 0L) {
            LlamaNative.nativeFree(handle)
            handle = 0L
        }
    }

    companion object {
        private const val REASON_LENGTH = 1

        /** Loads a GGUF file. Throws [IllegalStateException] when llama.cpp cannot load it on this device. */
        fun load(path: String, contextTokens: Int, threads: Int): LlamaEngine {
            LlamaNative.ensureLoaded()
            val handle = LlamaNative.nativeLoad(path, contextTokens, threads)
            return LlamaEngine(handle, LlamaNative.nativeContextTokens(handle))
        }
    }
}

/** Receives reply bytes from native code. Return false to stop the reply. */
fun interface TokenCallback {
    fun onToken(bytes: ByteArray): Boolean
}

internal object LlamaNative {
    @Volatile
    private var loaded = false

    @Synchronized
    fun ensureLoaded() {
        if (loaded) return
        try {
            System.loadLibrary("modelmistress_llama")
        } catch (error: UnsatisfiedLinkError) {
            throw IllegalStateException(
                "llama.cpp is not available on this device. It needs an arm64 processor.",
                error,
            )
        }
        loaded = true
    }

    @JvmStatic
    external fun nativeLoad(path: String, contextTokens: Int, threads: Int): Long

    @JvmStatic
    external fun nativeContextTokens(handle: Long): Int

    @JvmStatic
    external fun nativeFree(handle: Long)

    @JvmStatic
    external fun nativeGenerate(
        handle: Long,
        roles: Array<ByteArray>,
        contents: Array<ByteArray>,
        maxNewTokens: Int,
        temperature: Float,
        topP: Float,
        topK: Int,
        seed: Long,
        callback: TokenCallback,
    ): IntArray
}
