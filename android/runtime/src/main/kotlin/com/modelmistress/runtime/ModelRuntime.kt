package com.modelmistress.runtime

import android.content.Context
import android.util.Log
import com.modelmistress.core.agent.AgentService
import com.modelmistress.core.agent.AgentSettings
import com.modelmistress.core.agent.Approval
import com.modelmistress.core.agent.Approvals
import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.chat.ChatStore
import com.modelmistress.core.chat.FileChatStore
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.chat.ModelRouter
import com.modelmistress.core.misty.FileMistyFeedback
import com.modelmistress.core.misty.MistyFeedback
import com.modelmistress.core.remote.CombinedCatalog
import com.modelmistress.core.environment.EnvironmentStore
import com.modelmistress.runtime.overlay.TransferdOverlay
import com.modelmistress.server.ModelMistressServer
import com.modelmistress.server.ModelUploads
import com.modelmistress.server.RouterServices
import com.modelmistress.server.ServerConfig
import java.io.File
import java.io.IOException
import java.io.InputStream
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

    /**
     * The sandboxes: one folder per chat, on this device. A chat's tools reach its own folder and the ones it is linked to,
     * and nothing else on the device.
     */
    val environments = EnvironmentStore(context.filesDir)

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

    private val approvalState = MutableStateFlow<List<Approval>>(emptyList())

    /** The owner's OK for what other apps ask to do here. A request waits in this list until the owner answers or it times out. */
    val approvals = Approvals(onChange = { waiting -> approvalState.value = waiting })

    /** The requests waiting for the owner, oldest first. The router's screen shows them. */
    val pendingApprovals: StateFlow<List<Approval>> = approvalState.asStateFlow()

    /** The chat and Misty answers the router gives other apps, with the owner's tool policy and each chat's sandbox. */
    val agent = AgentService(
        chats = chats,
        environments = environments,
        feedback = feedback,
        approvals = approvals,
        settings = { AgentSettings(settings.toolUse, settings.fileWrites, settings.mistyEnabled) },
        reflex = { if (misty.reflexReady(settings.mistyReflex)) NeedleReflex(needle) else null },
        deviceStatus = { misty.status() },
        installedModels = { misty.installedModels() },
    )

    /** What the router's `/mm/v1` API reads and changes on this phone. */
    val host = RouterHost(context, settings, library, hub, catalog, downloads, needle, benchmarks)

    private val state = MutableStateFlow(ServerState())
    val serverState: StateFlow<ServerState> = state.asStateFlow()

    private var server: ModelMistressServer? = null

    /** The model files another ModelMistress sends this phone, stored in the library (see ModelUploads). */
    private fun receivedModels(): ModelUploads = object : ModelUploads {
        override fun receive(name: String, body: InputStream, bytes: Long): String = try {
            library.receive(name, body, bytes).id
        } catch (error: NoRoom) {
            throw ApiException(507, "no_space", error.message.orEmpty())
        } catch (error: IllegalArgumentException) {
            throw ApiException(400, "invalid_model", error.message.orEmpty())
        } catch (error: IOException) {
            throw ApiException(500, "upload_failed", "The model could not be saved on this device: ${error.message}")
        }
    }

    /** Starts the server with the current settings and returns the state it is in afterwards. */
    @Synchronized
    fun startServer(): ServerState {
        if (server?.isRunning == true) return state.value
        val address = if (settings.allowNetwork) "0.0.0.0" else "127.0.0.1"
        val config = ServerConfig(host = address, port = settings.port, apiKey = settings.apiKey)
        val services = RouterServices(
            agent = agent,
            host = this.host,
            approvals = approvals,
            chats = chats,
            environments = environments,
            feedback = feedback,
            version = ROUTER_VERSION,
            images = imageHub.models().isNotEmpty(),
        )
        val created = ModelMistressServer(
            router = ModelRouter(catalog),
            config = config,
            images = imageHub,
            services = services,
            log = { line -> Log.i(TAG, line) },
            uploads = receivedModels(),
        )
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

        /** The router's version, as its API reports it. */
        const val ROUTER_VERSION = "0.1.0"

        @Volatile
        private var instance: ModelRuntime? = null

        fun get(context: Context): ModelRuntime = instance ?: synchronized(this) {
            instance ?: ModelRuntime(context.applicationContext).also { instance = it }
        }
    }
}
