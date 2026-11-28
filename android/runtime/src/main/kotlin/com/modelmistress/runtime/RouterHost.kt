package com.modelmistress.runtime

import android.app.ActivityManager
import android.content.Context
import android.util.Log
import com.modelmistress.cactus.CactusNeedle
import com.modelmistress.core.bench.BenchmarkCodec
import com.modelmistress.core.bench.BenchmarkReport
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.models.HfSpec
import com.modelmistress.core.models.HuggingFaceFiles
import com.modelmistress.core.mm.MmDevice
import com.modelmistress.core.mm.MmDownload
import com.modelmistress.core.mm.MmHost
import com.modelmistress.core.mm.MmModel
import com.modelmistress.core.mm.MmSettings
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/**
 * The router's `/mm/v1` operations on this phone. The server asks the owner before it calls any of the ones that change the
 * device, so nothing here asks again. Reads are answered from the same stores the app's screens use.
 */
class RouterHost(
    private val context: Context,
    private val settings: RuntimeSettings,
    private val library: ModelLibrary,
    private val hub: EngineHub,
    private val catalog: ModelCatalog,
    private val downloads: ModelDownloads,
    private val needle: NeedleStore,
    private val benchmarks: BenchmarkStore,
) : MmHost {
    private val benchmarkRunning = AtomicBoolean(false)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    override fun settings(): MmSettings = MmSettings(
        toolUse = settings.toolUse.name,
        fileWrites = settings.fileWrites.name,
        mistyEnabled = settings.mistyEnabled,
        mistyReflex = settings.mistyReflex,
        defaultModel = settings.defaultModelId,
        allowNetwork = settings.allowNetwork,
        port = settings.port,
    )

    override fun device(): MmDevice {
        val info = ActivityManager.MemoryInfo()
        (context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager)?.getMemoryInfo(info)
        return MmDevice(
            totalMemoryBytes = info.totalMem,
            freeMemoryBytes = info.availMem,
            needleReady = CactusNeedle.runtimeAvailable() && needle.isInstalled(),
            loadedModel = hub.loadedModelId(),
        )
    }

    override fun models(): List<MmModel> {
        val local = library.list().map { it.id }.toSet()
        val default = settings.defaultModelId
        return catalog.models().map { model ->
            MmModel(
                id = model.id,
                kind = if (model.id in local) "local" else "desktop",
                sizeBytes = model.sizeBytes,
                contextTokens = model.contextTokens,
                isDefault = model.id == default,
                description = model.description,
            )
        }
    }

    override fun setDefault(id: String): Boolean {
        if (library.find(id) == null) return false
        settings.defaultModelId = id
        return true
    }

    override fun deleteModel(id: String): Boolean {
        if (library.find(id) == null) return false
        hub.unloadModel(id)
        library.delete(id)
        if (settings.defaultModelId == id) settings.defaultModelId = null
        return true
    }

    /** Only the single-file GGUF models a download can take. Waits for the network. */
    override fun repoFiles(repo: String): List<String> {
        val listing = downloads.listFiles(HfSpec.parse(repo))
        return HuggingFaceFiles.candidates(listing.files).map { it.path }
    }

    override fun startDownload(repo: String, file: String): MmDownload {
        val spec = HfSpec.parse(repo)
        val listing = downloads.listFiles(spec)
        val picked = HuggingFaceFiles.candidates(listing.files)
            .firstOrNull { it.path == file || it.name == file }
            ?: throw IllegalArgumentException("$repo has no model file '$file'. List the repo's files first.")
        if (!downloads.start(spec, listing.revision, picked)) {
            throw IllegalStateException("A download is already running.")
        }
        return current() ?: throw IllegalStateException("The download did not start.")
    }

    override fun downloads(): List<MmDownload> = listOfNotNull(current())

    override fun stopDownloads() {
        downloads.stop()
    }

    override fun benchmarkReport(): String? = benchmarks.load()?.let(BenchmarkCodec::encode)

    /** Runs the stress test and times each local model, as the app's benchmark screen does. Runs in the background. */
    override fun startBenchmark(): Boolean {
        if (!benchmarkRunning.compareAndSet(false, true)) return false
        scope.launch {
            try {
                val ids = library.list().map { it.id }
                val stress = runCatching { StressTester(context).run(onProgress = {}) }.getOrNull()
                val timed = ModelBench(hub, library).run(ids) {}
                benchmarks.save(BenchmarkReport(System.currentTimeMillis(), PhoneProfile.read(context), stress, timed.runs))
            } catch (error: Exception) {
                Log.w(TAG, "The benchmark did not finish.", error)
            } finally {
                benchmarkRunning.set(false)
            }
        }
        return true
    }

    private fun current(): MmDownload? {
        val state = downloads.states.value
        val file = state.file ?: return null
        if (state.phase == DownloadPhase.IDLE) return null
        val repo = state.repo.orEmpty()
        return MmDownload(
            id = "$repo:$file",
            repo = repo,
            file = file,
            phase = state.phase.name.lowercase(),
            done = state.done,
            total = state.total,
            message = state.message,
        )
    }

    private companion object {
        const val TAG = "ModelMistress"
    }
}
