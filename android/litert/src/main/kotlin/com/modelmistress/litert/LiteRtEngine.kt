package com.modelmistress.litert

import com.google.ai.edge.litertlm.Backend
import com.google.ai.edge.litertlm.Content
import com.google.ai.edge.litertlm.Contents
import com.google.ai.edge.litertlm.ConversationConfig
import com.google.ai.edge.litertlm.Engine
import com.google.ai.edge.litertlm.EngineConfig
import com.google.ai.edge.litertlm.Message
import com.google.ai.edge.litertlm.SamplerConfig
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.LocalEngine
import com.modelmistress.core.chat.Role
import com.modelmistress.core.chat.TokenSink
import com.modelmistress.core.chat.splitForReply
import java.io.File
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.takeWhile
import kotlinx.coroutines.runBlocking

/**
 * One LiteRT-LM model loaded on this device. The engine stays loaded between replies. Each reply runs in a new conversation
 * that starts from the messages the app sends, so the history belongs to the app, not the engine. Replies run one at a time.
 */
class LiteRtEngine private constructor(
    private val engine: Engine,
) : LocalEngine {
    private var closed = false

    @Synchronized
    override fun complete(messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats {
        check(!closed) { "The model has been unloaded." }
        val turns = splitForReply(messages)
        val conversation = engine.createConversation(
            ConversationConfig(
                systemInstruction = turns.system?.let { Contents.of(it) },
                initialMessages = turns.earlier.map { turn ->
                    if (turn.role == Role.USER) Message.user(turn.content) else Message.model(turn.content)
                },
                samplerConfig = SamplerConfig(
                    topK = options.topK,
                    topP = options.topP.toDouble(),
                    temperature = options.temperature.toDouble(),
                ),
                maxOutputToken = options.maxTokens,
            ),
        )
        var stopped = false
        try {
            runBlocking {
                conversation.sendMessageAsync(turns.latest)
                    .takeWhile { !stopped }
                    .collect { message ->
                        val text = message.contents.contents
                            .filterIsInstance<Content.Text>()
                            .joinToString("") { it.text }
                        if (text.isNotEmpty() && !sink.onText(text)) {
                            stopped = true
                            conversation.cancelProcess()
                        }
                    }
            }
        } finally {
            conversation.close()
        }
        // LiteRT-LM does not report token counts or a length stop here, so the stats stay at zero.
        return GenerationStats(promptTokens = 0, completionTokens = 0, finishReason = FinishReason.STOP)
    }

    @Synchronized
    override fun close() {
        if (!closed) {
            closed = true
            engine.close()
        }
    }

    companion object {
        /**
         * Loads a `.litertlm` file with the CPU backend. Throws [IllegalStateException] when LiteRT-LM cannot load it on this
         * device. [cacheDirectory] holds what LiteRT-LM compiles, so a later load of the same model is faster.
         */
        fun load(path: String, cacheDirectory: File, contextTokens: Int, threads: Int): LiteRtEngine {
            cacheDirectory.mkdirs()
            val engine = try {
                Engine(
                    EngineConfig(
                        modelPath = path,
                        backend = Backend.CPU(threadCount = threads),
                        maxNumTokens = contextTokens,
                        cacheDir = cacheDirectory.absolutePath,
                    ),
                )
            } catch (error: Exception) {
                throw IllegalStateException("The LiteRT-LM model could not be read on this device.", error)
            }
            try {
                engine.initialize()
            } catch (error: Exception) {
                engine.close()
                throw IllegalStateException("The LiteRT-LM model could not be loaded on this device.", error)
            }
            return LiteRtEngine(engine)
        }
    }
}
