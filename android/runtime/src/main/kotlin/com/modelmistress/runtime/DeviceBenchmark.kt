package com.modelmistress.runtime

import android.app.ActivityManager
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.os.Build
import android.os.PowerManager
import com.modelmistress.core.bench.BenchmarkCodec
import com.modelmistress.core.bench.BenchmarkReport
import com.modelmistress.core.bench.DeviceProfile
import com.modelmistress.core.bench.ModelRun
import com.modelmistress.core.bench.StressResult
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.LocalEngine
import com.modelmistress.core.chat.Role
import java.io.File
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicLongArray
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** What this phone is, read from Android. */
object PhoneProfile {
    fun read(context: Context): DeviceProfile {
        val manager = context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager
        val memory = ActivityManager.MemoryInfo().also { manager?.getMemoryInfo(it) }
        return DeviceProfile(
            model = listOfNotNull(Build.MANUFACTURER, Build.MODEL).joinToString(" ").trim(),
            androidSdk = Build.VERSION.SDK_INT,
            cores = Runtime.getRuntime().availableProcessors(),
            ramBytes = memory.totalMem,
            abi = Build.SUPPORTED_ABIS.firstOrNull() ?: "unknown",
            dotProd = hasDotProd(),
        )
    }

    /** The CPU's feature list names "asimddp" when it has the dot-product instructions the native engine uses. */
    private fun hasDotProd(): Boolean = runCatching {
        File("/proc/cpuinfo").readLines().any { line -> line.startsWith("Features") && "asimddp" in line }
    }.getOrDefault(false)
}

/** The battery temperature and the thermal state, when Android reports them. */
object PhoneTemperature {
    fun batteryCelsius(context: Context): Double? {
        val intent = context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED)) ?: return null
        val tenths = intent.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, -1)
        return if (tenths < 0) null else tenths / 10.0
    }

    fun thermalStatus(context: Context): Int? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
        val power = context.getSystemService(Context.POWER_SERVICE) as? PowerManager ?: return null
        return power.currentThermalStatus
    }
}

/**
 * Holds the CPU at full load on every core for [seconds], then copies memory for a few seconds. The CPU rate is counted each
 * second, so the first and last few seconds show whether the phone slows down under sustained load.
 */
class StressTester(private val context: Context) {
    @Volatile
    private var sink = 0.0

    suspend fun run(seconds: Int = CPU_SECONDS, onProgress: (String) -> Unit): StressResult = withContext(Dispatchers.Default) {
        val threads = Runtime.getRuntime().availableProcessors().coerceIn(1, 8)
        val batteryStart = PhoneTemperature.batteryCelsius(context)
        val buckets = AtomicLongArray(seconds)
        val start = System.nanoTime()
        val end = start + seconds * NANOS_PER_SECOND
        onProgress("CPU stress on $threads cores for $seconds seconds")
        coroutineScope {
            repeat(threads) {
                launch(Dispatchers.Default) {
                    var x = 1.000001 + it
                    while (isActive) {
                        val now = System.nanoTime()
                        if (now >= end) break
                        val bucket = ((now - start) / NANOS_PER_SECOND).toInt().coerceIn(0, seconds - 1)
                        for (i in 0 until BATCH) x = Math.sqrt(x * x + 1.0001) + 0.5
                        buckets.addAndGet(bucket, BATCH.toLong())
                    }
                    sink = x
                }
            }
        }
        val counts = List(seconds) { buckets.get(it).toDouble() }
        val window = minOf(WINDOW_SECONDS, seconds)
        val first = counts.take(window).average()
        val last = counts.takeLast(window).average()
        val cpuRate = counts.average()

        onProgress("Memory copy test")
        val source = ByteArray(COPY_BYTES) { 1 }
        val target = ByteArray(COPY_BYTES)
        val copyStart = System.nanoTime()
        val copyEnd = copyStart + COPY_SECONDS * NANOS_PER_SECOND
        val copied = AtomicLong(0)
        while (System.nanoTime() < copyEnd && isActive) {
            source.copyInto(target)
            copied.addAndGet(COPY_BYTES.toLong())
        }
        val copySeconds = (System.nanoTime() - copyStart) / NANOS_PER_SECOND.toDouble()
        val megabytesPerSecond = copied.get() / 1_048_576.0 / copySeconds.coerceAtLeast(0.001)

        StressResult(
            threads = threads,
            cpuOpsPerSecond = cpuRate,
            memoryMegabytesPerSecond = megabytesPerSecond,
            durationSeconds = seconds.toDouble(),
            firstWindowOps = first,
            lastWindowOps = last,
            batteryTempStartC = batteryStart,
            batteryTempEndC = PhoneTemperature.batteryCelsius(context),
            thermalStatus = PhoneTemperature.thermalStatus(context),
        )
    }

    companion object {
        const val CPU_SECONDS = 20
        private const val WINDOW_SECONDS = 3
        private const val BATCH = 2000
        private const val COPY_SECONDS = 3L
        private const val COPY_BYTES = 32 * 1024 * 1024
        private const val NANOS_PER_SECOND = 1_000_000_000L
    }
}

/**
 * Times each model with a fixed prompt, greedy decoding and a fixed number of new tokens. Each model is loaded for its turn
 * and stays loaded until the next model or chat asks for another.
 */
class ModelBench(private val hub: EngineHub, private val library: ModelLibrary) {
    /** The timed runs, and a line for each model that could not be timed. */
    data class Result(val runs: List<ModelRun>, val failures: List<String>)

    fun run(modelIds: List<String>, onProgress: (String) -> Unit): Result {
        val runs = mutableListOf<ModelRun>()
        val failures = mutableListOf<String>()
        for (id in modelIds) {
            onProgress("Timing $id")
            val bytes = library.find(id)?.sizeBytes ?: 0L
            try {
                runs += hub.withEngine(id) { engine -> measure(id, bytes, engine) }
            } catch (error: Exception) {
                failures += "$id: ${error.message ?: "could not be timed"}"
            }
        }
        return Result(runs, failures)
    }

    private fun measure(id: String, bytes: Long, engine: LocalEngine): ModelRun {
        val messages = listOf(ChatMessage(Role.USER, PROMPT))
        val options = GenerationOptions(maxTokens = NEW_TOKENS, temperature = 0f, seed = 7L)
        val start = System.nanoTime()
        var firstTextAt = -1L
        val stats = engine.complete(messages, options) { text ->
            if (firstTextAt < 0 && text.isNotEmpty()) firstTextAt = System.nanoTime()
            true
        }
        val end = System.nanoTime()
        val firstAt = if (firstTextAt < 0) end else firstTextAt
        return ModelRun(
            modelId = id,
            fileBytes = bytes,
            promptTokens = stats.promptTokens,
            generatedTokens = stats.completionTokens,
            timeToFirstTokenMs = (firstAt - start) / 1_000_000L,
            totalMs = (end - start) / 1_000_000L,
        )
    }

    companion object {
        const val NEW_TOKENS = 96
        private val PARAGRAPH = "A phone runs its chat model on the processor alone. The model reads every word of the prompt " +
            "before it writes, and then writes one token at a time. Speed depends on how much memory the model reads for each " +
            "token and on how fast the phone can read it. "
        private val PROMPT = (PARAGRAPH.repeat(4)) + "In two sentences, explain what limits how fast the answer appears."
    }
}

/** The latest benchmark report, kept on the device as JSON. */
class BenchmarkStore(private val file: File) {
    fun load(): BenchmarkReport? = runCatching {
        if (file.isFile) BenchmarkCodec.decode(file.readText()) else null
    }.getOrNull()

    fun save(report: BenchmarkReport) {
        file.parentFile?.mkdirs()
        file.writeText(BenchmarkCodec.encode(report))
    }
}
