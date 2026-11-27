package com.modelmistress.runtime

import android.app.ActivityManager
import android.content.Context
import com.modelmistress.cactus.CactusNeedle
import com.modelmistress.cactus.CactusNeedleModel
import com.modelmistress.core.chat.ModelInfo
import com.modelmistress.core.misty.MistyReflex
import com.modelmistress.core.models.FileDownloader
import com.modelmistress.core.models.HuggingFaceClient
import java.io.File
import java.io.IOException

/**
 * Needle 3 on this device: fetched once from its pinned Hugging Face revision, checked against its size and SHA-256, and
 * then used by Misty's reflex. A copy that does not match is never kept.
 */
class NeedleStore(private val directory: File) {
    /** The installed model file. Only meaningful after [isInstalled] returns true. */
    val file: File get() = File(directory, CactusNeedleModel.FILE_NAME)

    private val partial: File get() = File(directory, CactusNeedleModel.FILE_NAME + ".part")

    /** The checksum result for the file as it was last seen, so the hash is not recomputed on every turn. */
    private var checked: Pair<Long, Boolean>? = null

    /** True when the file is present, the right size, and its checksum matches the pinned one. */
    @Synchronized
    fun isInstalled(): Boolean {
        val model = file
        if (!model.isFile || model.length() != CactusNeedleModel.SIZE_BYTES) return false
        val stamp = model.lastModified()
        checked?.takeIf { it.first == stamp }?.let { return it.second }
        val matches = FileDownloader.sha256Of(model) == CactusNeedleModel.SHA256
        checked = stamp to matches
        return matches
    }

    /**
     * Fetches Needle into this store, resuming a part-finished copy. [progress] gets the bytes held and the total.
     * [keepGoing] is asked before each chunk; returning false stops it with [com.modelmistress.core.models.DownloadStopped].
     * A copy that fails the size or checksum check is thrown away by the downloader.
     */
    @Synchronized
    fun download(
        client: HuggingFaceClient,
        progress: (done: Long, total: Long) -> Unit,
        keepGoing: () -> Boolean,
    ) {
        directory.mkdirs()
        FileDownloader.fetch(
            client = client,
            url = CactusNeedleModel.DOWNLOAD_URL,
            expectedSize = CactusNeedleModel.SIZE_BYTES,
            expectedSha256 = CactusNeedleModel.SHA256,
            partial = partial,
            progress = progress,
            keepGoing = keepGoing,
        )
        file.delete()
        if (!partial.renameTo(file)) {
            partial.delete()
            throw IOException("Needle could not be saved on this device.")
        }
        checked = null
    }

    /** Removes the installed copy and any part-finished one. */
    @Synchronized
    fun delete() {
        file.delete()
        partial.delete()
        checked = null
    }
}

/** The Misty reflex: Needle proposes one tool call as raw JSON, which Misty checks before anything runs. */
class NeedleReflex(private val store: NeedleStore) : MistyReflex {
    override fun propose(input: String, toolsJson: String): String {
        check(store.isInstalled()) { "Needle is not installed." }
        return CactusNeedle.propose(store.file, input, toolsJson)
    }
}

/**
 * What Misty knows about this phone: free memory, whether Needle is ready, and which models are installed. The status and
 * model list are the tools the chat model can ask for.
 */
class MistyFacts(
    private val context: Context,
    private val needle: NeedleStore,
    private val chatModels: () -> List<ModelInfo>,
) {
    /** True when Needle is installed, the Cactus runtime loads on this ABI, and the owner turned the reflex on. */
    fun reflexReady(enabled: Boolean): Boolean =
        enabled && CactusNeedle.runtimeAvailable() && needle.isInstalled()

    /** A short report for the device_status tool. */
    fun status(): String {
        val freeGb = freeMemoryBytes() / BYTES_PER_GB
        val needleState = when {
            !CactusNeedle.runtimeAvailable() -> "not available on this Android ABI"
            needle.isInstalled() -> "installed and ready"
            else -> "not installed"
        }
        return "Free memory: ${String.format(java.util.Locale.US, "%.1f", freeGb)} GB. Needle 3: $needleState."
    }

    /** The models installed on this device, for the models_list tool. */
    fun installedModels(): String {
        val chat = chatModels().map { model ->
            "chat: ${model.id} (${model.sizeBytes / BYTES_PER_MB} MB)"
        }
        val cactus = if (needle.isInstalled()) listOf("cactus: Needle 3") else emptyList()
        return (chat + cactus).ifEmpty { listOf("No models are installed.") }.joinToString("\n")
    }

    private fun freeMemoryBytes(): Long {
        val manager = context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager ?: return 0L
        val info = ActivityManager.MemoryInfo()
        manager.getMemoryInfo(info)
        return info.availMem
    }

    private companion object {
        const val BYTES_PER_GB = 1024.0 * 1024.0 * 1024.0
        const val BYTES_PER_MB = 1_000_000L
    }
}
