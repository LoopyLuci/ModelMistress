package com.modelmistress.app

import android.content.Context
import android.content.Intent
import android.speech.RecognizerIntent
import android.speech.tts.TextToSpeech
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

/** Reads a reply aloud with the phone's text-to-speech engine. Speaking a new reply stops the one before. */
class Speaker(context: Context) {
    private var engine: TextToSpeech? = null
    private var ready = false
    private var waiting: String? = null

    init {
        engine = TextToSpeech(context) { status ->
            val tts = engine ?: return@TextToSpeech
            ready = status == TextToSpeech.SUCCESS
            if (ready) {
                val language = tts.setLanguage(Locale.getDefault())
                if (language == TextToSpeech.LANG_MISSING_DATA || language == TextToSpeech.LANG_NOT_SUPPORTED) {
                    tts.setLanguage(Locale.US)
                }
                waiting?.let { speak(it) }
                waiting = null
            }
        }
    }

    /** Says [text] now. Returns false when the phone has no speech engine ready. */
    fun speak(text: String): Boolean {
        val tts = engine ?: return false
        if (!ready) {
            waiting = text
            return true
        }
        return tts.speak(text, TextToSpeech.QUEUE_FLUSH, null, "reply") == TextToSpeech.SUCCESS
    }

    fun stop() {
        engine?.stop()
    }

    fun shutdown() {
        engine?.stop()
        engine?.shutdown()
        engine = null
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
