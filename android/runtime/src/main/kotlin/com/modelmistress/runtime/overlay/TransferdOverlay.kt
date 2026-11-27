package com.modelmistress.runtime.overlay

import android.content.Context
import com.modelmistress.core.json.Json
import com.modelmistress.core.json.JsonException
import com.modelmistress.core.remote.OverlayEndpoints
import java.io.File
import java.io.IOException

/** What the overlay reports about this phone's node. [error] is set when the node could not be read. */
data class OverlayStatus(
    val running: Boolean,
    val nodeId: String? = null,
    val relays: List<String> = emptyList(),
    val error: String? = null,
)

/**
 * A desktop that this phone has just paired with through its invite. [relaysToAdd] are relays the desktop uses that this
 * phone is not yet registered on; the overlay registers on them when the app starts next, so they need a restart.
 */
data class OverlayPairing(val nodeId: String, val relaysToAdd: List<String>)

/**
 * This phone's Transferd overlay node, run inside the app. It reaches a paired desktop's hub from any network, over the
 * overlay, with no LAN or Tailscale address: the desktop publishes its hub to paired nodes only (`mm-overlay serve`),
 * and [hubRoot] gives a loopback address on this phone that the overlay forwards to that hub.
 *
 * One node runs per process, started on first use. Its identity, state and audit trail are kept in the app's private
 * files folder. The node dials out only; it opens no inbound port.
 */
class TransferdOverlay(context: Context) : OverlayEndpoints {
    private val appContext = context.applicationContext
    private val home = File(appContext.filesDir, "overlay")
    private val prefs = appContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    private val lock = Any()

    /** The relays the running node registered on, once it has started in this process. */
    private var startedWith: List<String>? = null

    /** The overlay node id of this phone, starting the node if it is not running yet. */
    fun nodeId(): String {
        val answer = ensureRunning()
        return answer["nodeId"] as? String ?: throw IOException("The overlay did not report its node id.")
    }

    /** Pairs with the desktop that made [invite]. [label] is the name the desktop is saved under on this phone. */
    fun pair(invite: String, label: String): OverlayPairing {
        ensureRunning()
        val answer = call { OverlayNative.accept(invite.trim(), label.trim()) }
        val nodeId = answer["nodeId"] as? String ?: throw IOException("The overlay did not report the desktop's id.")
        val theirs = (answer["relays"] as? List<*>).orEmpty().filterIsInstance<String>()
        val known = savedRelays()
        val merged = known + theirs.filterNot { it in known }
        if (merged != known) saveRelays(merged)
        val started = startedWith.orEmpty()
        return OverlayPairing(nodeId = nodeId, relaysToAdd = merged.filterNot { it in started })
    }

    /** The loopback root, `http://127.0.0.1:<port>`, of the hub on the desktop with [overlayId]. */
    override fun hubRoot(overlayId: String): String {
        ensureRunning()
        val answer = call { OverlayNative.connect(overlayId, HUB_SERVICE) }
        val addr = answer["addr"] as? String ?: throw IOException("The overlay did not give an address for the desktop.")
        return "http://$addr"
    }

    /** Whether the node is running, and what it is registered on. Never starts the node. */
    fun status(): OverlayStatus = try {
        val answer = call { OverlayNative.status() }
        OverlayStatus(
            running = answer["running"] == true,
            nodeId = answer["nodeId"] as? String,
            relays = (answer["relays"] as? List<*>).orEmpty().filterIsInstance<String>(),
        )
    } catch (error: IOException) {
        OverlayStatus(running = false, error = error.message)
    }

    /** Relays this phone is set to register on. Pairing adds the relays a desktop's invite names. */
    fun relays(): List<String> = savedRelays()

    private fun ensureRunning(): Map<*, *> = synchronized(lock) {
        val saved = savedRelays()
        home.mkdirs()
        val answer = call {
            OverlayNative.start(home.absolutePath, LABEL, saved.joinToString(","), "", "")
        }
        if (startedWith == null) startedWith = saved
        answer
    }

    private fun savedRelays(): List<String> =
        try {
            prefs.getString(KEY_RELAYS, null).orEmpty().split(',').map { it.trim() }.filter { it.isNotEmpty() }
        } catch (error: RuntimeException) {
            emptyList()
        }

    private fun saveRelays(relays: List<String>) {
        prefs.edit().putString(KEY_RELAYS, relays.joinToString(",")).apply()
    }

    /** Runs one overlay call and returns its JSON object. A failure becomes an [IOException] with the overlay's message. */
    private fun call(invoke: () -> String?): Map<*, *> {
        val text = try {
            invoke()
        } catch (error: LinkageError) {
            throw IOException("This build has no Transferd overlay library (${error.message}).")
        }
        val answer = try {
            Json.parse(text.orEmpty()) as? Map<*, *>
        } catch (error: JsonException) {
            null
        } ?: throw IOException("The overlay answered in a form this app cannot read.")
        if (answer["ok"] != true) {
            throw IOException(answer["error"] as? String ?: "The overlay failed.")
        }
        return answer
    }

    companion object {
        /** The service a desktop publishes its hub under. Must match `mm-overlay`'s HUB_SERVICE. */
        const val HUB_SERVICE = "modelmistress-hub"

        /** The name this phone's node goes by in invites. */
        private const val LABEL = "ModelMistress phone"
        private const val PREFS = "overlay"
        private const val KEY_RELAYS = "relays"
    }
}
