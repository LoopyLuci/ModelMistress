package com.modelmistress.runtime

import android.content.Context
import android.util.Base64
import java.security.SecureRandom

/** The owner's settings for the local server and the models, kept on this device only. */
class RuntimeSettings(context: Context) {
    private val prefs = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

    var port: Int
        get() = prefs.getInt(KEY_PORT, DEFAULT_PORT).takeIf { it in 1..65535 } ?: DEFAULT_PORT
        set(value) {
            prefs.edit().putInt(KEY_PORT, value.coerceIn(1, 65535)).apply()
        }

    /** Off: only apps on this device can reach the server. On: other devices on the network can, with the key. */
    var allowNetwork: Boolean
        get() = prefs.getBoolean(KEY_ALLOW_NETWORK, false)
        set(value) {
            prefs.edit().putBoolean(KEY_ALLOW_NETWORK, value).apply()
        }

    /** The bearer token every request except health must send. It is created on first use. */
    val apiKey: String
        get() = prefs.getString(KEY_API_KEY, null) ?: rotateApiKey()

    fun rotateApiKey(): String {
        val bytes = ByteArray(KEY_BYTES).also { SecureRandom().nextBytes(it) }
        val key = Base64.encodeToString(bytes, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)
        prefs.edit().putString(KEY_API_KEY, key).apply()
        return key
    }

    /** Tokens of context each model is loaded with. Larger uses more memory. */
    var contextTokens: Int
        get() = prefs.getInt(KEY_CONTEXT, DEFAULT_CONTEXT).coerceIn(256, 8192)
        set(value) {
            prefs.edit().putInt(KEY_CONTEXT, value.coerceIn(256, 8192)).apply()
        }

    /** CPU threads llama.cpp uses for a reply. */
    var threads: Int
        get() = prefs.getInt(KEY_THREADS, defaultThreads()).coerceIn(1, 8)
        set(value) {
            prefs.edit().putInt(KEY_THREADS, value.coerceIn(1, 8)).apply()
        }

    /** The model that answers when a request names none, or names "auto". */
    var defaultModelId: String?
        get() = prefs.getString(KEY_DEFAULT_MODEL, null)
        set(value) {
            prefs.edit().putString(KEY_DEFAULT_MODEL, value).apply()
        }

    /**
     * A Hugging Face token, needed only for gated or private repos. It stays on this device, in plain text, like the peer
     * keys do. Blank means none.
     */
    var hfToken: String?
        get() = prefs.getString(KEY_HF_TOKEN, null)?.takeIf { it.isNotBlank() }
        set(value) {
            prefs.edit().putString(KEY_HF_TOKEN, value?.trim()?.takeIf { it.isNotEmpty() }).apply()
        }

    /** Off: chats go straight to the chat model. On: chats go through Misty, with the owner's tools. */
    var mistyEnabled: Boolean
        get() = prefs.getBoolean(KEY_MISTY_ENABLED, false)
        set(value) {
            prefs.edit().putBoolean(KEY_MISTY_ENABLED, value).apply()
        }

    /** Whether Needle gets a first try at an obvious tool call. It only runs when Misty is on and Needle is installed. */
    var mistyReflex: Boolean
        get() = prefs.getBoolean(KEY_MISTY_REFLEX, true)
        set(value) {
            prefs.edit().putBoolean(KEY_MISTY_REFLEX, value).apply()
        }

    /** On: Misty asks before each file write. Off: Misty's file writes are refused. */
    var mistyAskBeforeWrite: Boolean
        get() = prefs.getBoolean(KEY_MISTY_ASK_WRITE, true)
        set(value) {
            prefs.edit().putBoolean(KEY_MISTY_ASK_WRITE, value).apply()
        }

    companion object {
        const val DEFAULT_PORT = 8080
        const val DEFAULT_CONTEXT = 2048

        private const val PREFERENCES = "modelmistress"
        private const val KEY_PORT = "port"
        private const val KEY_ALLOW_NETWORK = "allow_network"
        private const val KEY_API_KEY = "api_key"
        private const val KEY_CONTEXT = "context_tokens"
        private const val KEY_THREADS = "threads"
        private const val KEY_DEFAULT_MODEL = "default_model"
        private const val KEY_HF_TOKEN = "hf_token"
        private const val KEY_MISTY_ENABLED = "misty_enabled"
        private const val KEY_MISTY_REFLEX = "misty_reflex"
        private const val KEY_MISTY_ASK_WRITE = "misty_ask_write"
        private const val KEY_BYTES = 24

        private fun defaultThreads(): Int = (java.lang.Runtime.getRuntime().availableProcessors() - 1).coerceIn(1, 4)
    }
}
