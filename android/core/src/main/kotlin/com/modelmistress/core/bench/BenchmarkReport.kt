package com.modelmistress.core.bench

import com.modelmistress.core.json.Json

/** What the phone is, as the benchmark saw it. */
data class DeviceProfile(
    val model: String,
    val androidSdk: Int,
    val cores: Int,
    val ramBytes: Long,
    val abi: String,
    val dotProd: Boolean,
)

/**
 * The stress test: a CPU and memory workload held for a fixed time. [firstWindowOps] and [lastWindowOps] are the CPU rate
 * in the first and last part of the run, so a phone that slows down under sustained load shows it in [sustainedRatio].
 */
data class StressResult(
    val threads: Int,
    val cpuOpsPerSecond: Double,
    val memoryMegabytesPerSecond: Double,
    val durationSeconds: Double,
    val firstWindowOps: Double,
    val lastWindowOps: Double,
    val batteryTempStartC: Double?,
    val batteryTempEndC: Double?,
    val thermalStatus: Int?,
) {
    /** The share of its early speed the phone keeps by the end of the run. 1.0 means no slowdown. */
    val sustainedRatio: Double
        get() = if (firstWindowOps <= 0.0) 1.0 else (lastWindowOps / firstWindowOps).coerceIn(0.1, 1.0)
}

/** One model timed on this phone with a fixed prompt. Times are in milliseconds from the start of the reply. */
data class ModelRun(
    val modelId: String,
    val fileBytes: Long,
    val promptTokens: Int,
    val generatedTokens: Int,
    val timeToFirstTokenMs: Long,
    val totalMs: Long,
) {
    /** Tokens written per second after the first one. Zero when there was too little output to time. */
    val decodeTokensPerSecond: Double
        get() {
            val decodeMs = totalMs - timeToFirstTokenMs
            if (generatedTokens <= 1 || decodeMs <= 0) return 0.0
            return (generatedTokens - 1) * 1000.0 / decodeMs
        }

    /** Prompt tokens read per second. The first token includes the prompt, so this is an approximation. */
    val promptTokensPerSecond: Double
        get() = if (timeToFirstTokenMs <= 0) 0.0 else promptTokens * 1000.0 / timeToFirstTokenMs
}

/** One benchmark run on this phone: the stress test, then each model. Kept on the device. */
data class BenchmarkReport(
    val createdAt: Long,
    val device: DeviceProfile,
    val stress: StressResult?,
    val runs: List<ModelRun>,
)

/** Stores a [BenchmarkReport] as JSON. [decode] returns null for text that is not a report. */
object BenchmarkCodec {
    fun encode(report: BenchmarkReport): String = Json.write(
        mapOf(
            "createdAt" to report.createdAt,
            "device" to mapOf(
                "model" to report.device.model,
                "androidSdk" to report.device.androidSdk,
                "cores" to report.device.cores,
                "ramBytes" to report.device.ramBytes,
                "abi" to report.device.abi,
                "dotProd" to report.device.dotProd,
            ),
            "stress" to report.stress?.let { stress ->
                mapOf(
                    "threads" to stress.threads,
                    "cpuOpsPerSecond" to stress.cpuOpsPerSecond,
                    "memoryMegabytesPerSecond" to stress.memoryMegabytesPerSecond,
                    "durationSeconds" to stress.durationSeconds,
                    "firstWindowOps" to stress.firstWindowOps,
                    "lastWindowOps" to stress.lastWindowOps,
                    "batteryTempStartC" to stress.batteryTempStartC,
                    "batteryTempEndC" to stress.batteryTempEndC,
                    "thermalStatus" to stress.thermalStatus,
                )
            },
            "runs" to report.runs.map { run ->
                mapOf(
                    "modelId" to run.modelId,
                    "fileBytes" to run.fileBytes,
                    "promptTokens" to run.promptTokens,
                    "generatedTokens" to run.generatedTokens,
                    "timeToFirstTokenMs" to run.timeToFirstTokenMs,
                    "totalMs" to run.totalMs,
                )
            },
        ),
    )

    fun decode(text: String): BenchmarkReport? {
        val root = runCatching { Json.parse(text) }.getOrNull() as? Map<*, *> ?: return null
        val device = root["device"] as? Map<*, *> ?: return null
        return BenchmarkReport(
            createdAt = long(root, "createdAt") ?: return null,
            device = DeviceProfile(
                model = device["model"] as? String ?: return null,
                androidSdk = int(device, "androidSdk") ?: return null,
                cores = int(device, "cores") ?: return null,
                ramBytes = long(device, "ramBytes") ?: return null,
                abi = device["abi"] as? String ?: return null,
                dotProd = device["dotProd"] as? Boolean ?: false,
            ),
            stress = (root["stress"] as? Map<*, *>)?.let { stress ->
                StressResult(
                    threads = int(stress, "threads") ?: 0,
                    cpuOpsPerSecond = double(stress, "cpuOpsPerSecond") ?: 0.0,
                    memoryMegabytesPerSecond = double(stress, "memoryMegabytesPerSecond") ?: 0.0,
                    durationSeconds = double(stress, "durationSeconds") ?: 0.0,
                    firstWindowOps = double(stress, "firstWindowOps") ?: 0.0,
                    lastWindowOps = double(stress, "lastWindowOps") ?: 0.0,
                    batteryTempStartC = double(stress, "batteryTempStartC"),
                    batteryTempEndC = double(stress, "batteryTempEndC"),
                    thermalStatus = int(stress, "thermalStatus"),
                )
            },
            runs = (root["runs"] as? List<*>).orEmpty().mapNotNull { item ->
                val run = item as? Map<*, *> ?: return@mapNotNull null
                ModelRun(
                    modelId = run["modelId"] as? String ?: return@mapNotNull null,
                    fileBytes = long(run, "fileBytes") ?: return@mapNotNull null,
                    promptTokens = int(run, "promptTokens") ?: return@mapNotNull null,
                    generatedTokens = int(run, "generatedTokens") ?: return@mapNotNull null,
                    timeToFirstTokenMs = long(run, "timeToFirstTokenMs") ?: return@mapNotNull null,
                    totalMs = long(run, "totalMs") ?: return@mapNotNull null,
                )
            },
        )
    }

    private fun long(map: Map<*, *>, key: String): Long? = (map[key] as? Number)?.toLong()
    private fun int(map: Map<*, *>, key: String): Int? = (map[key] as? Number)?.toInt()
    private fun double(map: Map<*, *>, key: String): Double? = (map[key] as? Number)?.toDouble()
}
