package com.modelmistress.app

import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.speech.RecognizerIntent
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import java.util.Locale

/** The speech-to-text intent: the system's recognizer records and returns the words, so the app asks for no microphone permission. */
fun speechInputIntent(): Intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
    .putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
    .putExtra(RecognizerIntent.EXTRA_PROMPT, "Speak your message")
    .putExtra(RecognizerIntent.EXTRA_MAX_RESULTS, 1)

/**
 * Says text with the phone's text-to-speech engine. [speak] replaces what is being said, for reading a reply aloud. [enqueue]
 * adds a sentence to the end, for a live conversation.
 *
 * [onSpoken] is called once for each enqueued sentence, when it has been said, or when it could not be said. A sentence that
 * [stop] drops is not reported, so the caller can count what it has handed over. Everything here runs on the main thread: the
 * engine's callbacks are moved to it.
 */
class Speaker(context: Context, private val onSpoken: (() -> Unit)? = null) {
    private val main = Handler(Looper.getMainLooper())
    private var engine: TextToSpeech? = null
    private var ready = false
    private var failed = false
    private val waiting = mutableListOf<Triple<String, Int, String>>()

    /** The sentences handed over and not yet reported. A callback for anything else is ignored. */
    private val open = mutableSetOf<String>()
    private var sequence = 0

    /** An engine that never reports back is treated as missing, so the conversation does not wait for it forever. */
    private val giveUp = Runnable {
        if (!ready) fail()
    }

    init {
        engine = TextToSpeech(context) { status ->
            main.removeCallbacks(giveUp)
            val tts = engine ?: return@TextToSpeech
            if (status != TextToSpeech.SUCCESS) {
                fail()
                return@TextToSpeech
            }
            ready = true
            val language = tts.setLanguage(Locale.getDefault())
            if (language == TextToSpeech.LANG_MISSING_DATA || language == TextToSpeech.LANG_NOT_SUPPORTED) {
                tts.setLanguage(Locale.US)
            }
            tts.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                override fun onStart(utteranceId: String?) {}

                override fun onDone(utteranceId: String?) = finished(utteranceId)

                override fun onStop(utteranceId: String?, interrupted: Boolean) = finished(utteranceId)

                @Suppress("OVERRIDE_DEPRECATION")
                override fun onError(utteranceId: String?) = finished(utteranceId)

                override fun onError(utteranceId: String?, errorCode: Int) = finished(utteranceId)
            })
            val pending = waiting.toList()
            waiting.clear()
            for ((text, mode, id) in pending) {
                if (!say(text, mode, id)) finished(id)
            }
        }
        main.postDelayed(giveUp, INIT_LIMIT_MS)
    }

    /** Says [text] now, in place of anything being said. Returns false when the phone has no speech engine. */
    fun speak(text: String): Boolean {
        if (engine == null || failed) return false
        if (!ready) {
            waiting.removeAll { it.third == REPLY_ID }
            waiting += Triple(text, TextToSpeech.QUEUE_FLUSH, REPLY_ID)
            return true
        }
        return say(text, TextToSpeech.QUEUE_FLUSH, REPLY_ID)
    }

    /**
     * Adds [text] after what is being said. Returns false when the phone has no speech engine, or the engine refused the
     * sentence; nothing is reported for a sentence that returns false.
     */
    fun enqueue(text: String): Boolean {
        if (engine == null || failed) return false
        val id = LIVE_PREFIX + (++sequence)
        open += id
        if (!ready) {
            waiting += Triple(text, TextToSpeech.QUEUE_ADD, id)
            return true
        }
        if (say(text, TextToSpeech.QUEUE_ADD, id)) return true
        open -= id
        return false
    }

    /** True while sentences handed over are still being said. */
    fun busy(): Boolean = open.isNotEmpty()

    /** Stops what is being said, and drops what is queued. Nothing stopped is reported. */
    fun stop() {
        waiting.clear()
        open.clear()
        engine?.stop()
    }

    fun shutdown() {
        main.removeCallbacks(giveUp)
        waiting.clear()
        open.clear()
        engine?.stop()
        engine?.shutdown()
        engine = null
    }

    private fun say(text: String, mode: Int, id: String): Boolean {
        val tts = engine ?: return false
        return tts.speak(text, mode, null, id) == TextToSpeech.SUCCESS
    }

    /** The engine has no voice to give: everything handed over and not yet said is reported as done, and nothing else is taken. */
    private fun fail() {
        failed = true
        ready = false
        val lost = waiting.map { it.third }
        waiting.clear()
        lost.forEach { finished(it) }
    }

    /** Called from the engine's thread. The report is made on the main thread, and only once for each sentence. */
    private fun finished(id: String?) {
        main.post {
            if (id != null && open.remove(id)) onSpoken?.invoke()
        }
    }

    private companion object {
        const val REPLY_ID = "reply"
        const val LIVE_PREFIX = "live-"
        const val INIT_LIMIT_MS = 8_000L
    }
}

/** A [Speaker] for the screen that uses it, shut down when the screen leaves. */
@Composable
fun rememberSpeaker(): Speaker {
    val context = LocalContext.current.applicationContext
    val speaker = remember { Speaker(context) }
    DisposableEffect(speaker) {
        onDispose { speaker.shutdown() }
    }
    return speaker
}
