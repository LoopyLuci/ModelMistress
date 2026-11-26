package com.modelmistress.runtime

import android.content.Context
import android.util.Log
import com.modelmistress.core.chat.ChatStore
import com.modelmistress.core.chat.FileChatStore
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.chat.ModelRouter
import com.modelmistress.core.misty.FileMistyFeedback
import com.modelmistress.core.misty.MistyFeedback
import com.modelmistress.core.remote.CombinedCatalog
import com.modelmistress.core.tools.Workspace
import com.modelmistress.runtime.overlay.TransferdOverlay
import com.modelmistress.server.ModelMistressServer
import com.modelmistress.server.ServerConfig
import java.io.File
import java.io.IOException
import java.net.Inet4Address
import java.net.NetworkInterface
import java.util.Collections
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** What the local server is doing, for the screens to show. [baseUrl] is set while it runs. */
data class ServerState(
    val running: Boolean = false,
    val baseUrl: String? = null,
    val error: String? = null,
)

/**
 * The device's model runtime: the model library, the engine hub and the local server. One instance lives for the whole
 * process, so the app and the background service share the same models and server.
 */
class ModelRuntime private constructor(context: Context) {
    val settings = RuntimeSettings(context)
    val library = ModelLibrary(File(context.filesDir, "models"))
    val hub = EngineHub(library, settings, File(context.cacheDir, "litert"))

    /** The Stable Diffusion checkpoints this device makes images with. */
    val imageLibrary = ImageLibrary(File(context.filesDir, "image-models"))

    /** Makes the images that the server's image route asks for, one model loaded at a time. */
    val imageHub = ImageHub(imageLibrary, settings)

    /** The desktops this device is paired with. */
    val desktops = DesktopStore(context)

    /** The chats kept on this device, one file each under the app's storage. */
    val chats: ChatStore = FileChatStore(File(context.filesDir, "chats"))

    /** The text files the chat's tools may read. Only files added on the Models screen are visible to the model. */
    val workspace = Workspace(File(context.filesDir, "workspace").apply { mkdirs() })

    /** Downloads GGUF models from Hugging Face into the library, one at a time. */
    val downloads = ModelDownloads(library, settings)

    /** This phone's Transferd overlay node: how a desktop paired through the overlay is reached from any network. */
    val overlay = TransferdOverlay(context)

    /** Every model this device can answer with: its own, then each paired desktop's. The chat and the server use it. */
    val catalog: ModelCatalog = CombinedCatalog(hub, desktops::list, overlay)

    /** Needle 3, the Misty reflex's model, kept in app storage. */
    val needle = NeedleStore(File(context.filesDir, "cactus"))

    /** What Misty knows about this phone, read by its status and model tools. */
    val misty = MistyFacts(context, needle) { catalog.models() }

    /** The owner's ratings of replies. Misty routes similar questions by them, and they are kept for training later. */
    val feedback: MistyFeedback = FileMistyFeedback(File(context.filesDir, "misty/feedback.jsonl"))

    /** The latest device benchmark: the stress test and each model's speed on this phone. */
    val benchmarks = BenchmarkStore(File(context.filesDir, "benchmark/latest.json"))

    private val state = MutableStateFlow(ServerState())
    val serverState: StateFlow<ServerState> = state.asStateFlow()

    private var server: ModelMistressServer? = null

    /** Starts the server with the current settings and returns the state it is in afterwards. */
    @Synchronized
    fun startServer(): ServerState {
        if (server?.isRunning == true) return state.value
        val host = if (settings.allowNetwork) "0.0.0.0" else "127.0.0.1"
        val config = ServerConfig(host = host, port = settings.port, apiKey = settings.apiKey)
        val created = ModelMistressServer(ModelRouter(catalog), config, imageHub) { line -> Log.i(TAG, line) }
        try {
            val port = created.start()
            server = created
            state.value = ServerState(running = true, baseUrl = baseUrl(port), error = null)
        } catch (error: IOException) {
            created.close()
            state.value = ServerState(
                running = false,
                error = "The server could not start on port ${settings.port}: ${error.message}",
            )
        }
        return state.value
    }

    /** Stops the server and unloads the model. A reply already running finishes first. */
    @Synchronized
    fun stopServer() {
        server?.close()
        server = null
        hub.unloadAll()
        imageHub.unloadAll()
        state.value = ServerState()
    }

    private fun baseUrl(port: Int): String {
        val host = if (settings.allowNetwork) localAddress() ?: "0.0.0.0" else "127.0.0.1"
        return "http://$host:$port/v1"
    }

    private fun localAddress(): String? = runCatching {
        Collections.list(NetworkInterface.getNetworkInterfaces())
            .filter { it.isUp && !it.isLoopback }
            .flatMap { Collections.list(it.inetAddresses) }
            .firstOrNull { it is Inet4Address && !it.isLoopbackAddress && it.isSiteLocalAddress }
            ?.hostAddress
    }.getOrNull()

    companion object {
        private const val TAG = "ModelMistress"

        @Volatile
        private var instance: ModelRuntime? = null

        fun get(context: Context): ModelRuntime = instance ?: synchronized(this) {
            instance ?: ModelRuntime(context.applicationContext).also { instance = it }
        }
    }
}
