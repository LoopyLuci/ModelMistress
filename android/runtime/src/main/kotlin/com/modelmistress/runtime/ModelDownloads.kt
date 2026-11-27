package com.modelmistress.runtime

import com.modelmistress.core.models.DownloadStopped
import com.modelmistress.core.models.FileDownloader
import com.modelmistress.core.models.HfFile
import com.modelmistress.core.models.HfSpec
import com.modelmistress.core.models.HuggingFaceClient
import com.modelmistress.core.models.RepoFiles
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

enum class DownloadPhase { IDLE, DOWNLOADING, DONE, STOPPED, FAILED }

/** What the Models screen shows about the download in progress or the last one. [total] is 0 when the size is not known. */
data class DownloadState(
    val phase: DownloadPhase = DownloadPhase.IDLE,
    val file: String? = null,
    val done: Long = 0L,
    val total: Long = 0L,
    val message: String? = null,
)

/**
 * Downloads GGUF models from Hugging Face into the model library. One download runs at a time, in the background, and a
 * stopped one keeps its part on disk so the next start resumes it.
 */
class ModelDownloads(private val library: ModelLibrary, private val settings: RuntimeSettings) {
    private val state = MutableStateFlow(DownloadState())
    val states: StateFlow<DownloadState> = state.asStateFlow()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    @Volatile
    private var stopRequested = false

    /** Reads a repo's files. This waits for the network, so call it off the main thread. */
    fun listFiles(spec: HfSpec): RepoFiles = HuggingFaceClient(settings.hfToken).repoFiles(spec)

    /**
     * Starts the download of [file] from [spec] at [revision]. Returns false, and starts nothing, while another download is
     * running. A model already in the library is refused with a message rather than downloaded again.
     */
    fun start(spec: HfSpec, revision: String, file: HfFile): Boolean {
        if (state.value.phase == DownloadPhase.DOWNLOADING) return false
        val stem = file.name.removeSuffix(".gguf")
        if (library.targetFor(stem).exists()) {
            state.value = DownloadState(
                phase = DownloadPhase.FAILED,
                file = file.name,
                message = "${library.targetFor(stem).nameWithoutExtension} is already in the library.",
            )
            return true
        }
        stopRequested = false
        state.value = DownloadState(DownloadPhase.DOWNLOADING, file.name, 0L, file.sizeBytes, "Starting the download.")
        scope.launch {
            try {
                val partial = library.partialFor(stem)
                val held = if (partial.isFile) partial.length() else 0L
                val needed = (file.sizeBytes - held).coerceAtLeast(0L)
                if (library.usableBytes() < needed + ROOM_TO_SPARE) {
                    throw IllegalStateException(
                        "Not enough free space. This model needs about ${formatSize(needed)} more, plus room to spare.",
                    )
                }
                val client = HuggingFaceClient(settings.hfToken)
                FileDownloader.fetch(
                    client = client,
                    url = client.resolveUrl(spec, revision, file),
                    expectedSize = file.sizeBytes,
                    expectedSha256 = file.sha256,
                    partial = partial,
                    progress = { done, total -> state.update { it.copy(done = done, total = total) } },
                    keepGoing = { !stopRequested },
                )
                val model = library.install(partial, stem)
                state.value = DownloadState(
                    phase = DownloadPhase.DONE,
                    file = file.name,
                    done = file.sizeBytes,
                    total = file.sizeBytes,
                    message = "Installed ${model.id}.",
                )
            } catch (stopped: DownloadStopped) {
                state.update { it.copy(phase = DownloadPhase.STOPPED, message = stopped.message) }
            } catch (error: Exception) {
                state.update { it.copy(phase = DownloadPhase.FAILED, message = error.message ?: "The download failed.") }
            }
        }
        return true
    }

    /** Asks the running download to stop. What it fetched is kept for the next start. */
    fun stop() {
        stopRequested = true
    }

    private companion object {
        const val ROOM_TO_SPARE = 512L shl 20
    }
}
