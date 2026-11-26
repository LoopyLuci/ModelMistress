package com.modelmistress.core.bench

/**
 * A model this app can suggest, not yet on the phone. Sizes are the Hugging Face file sizes; the owner downloads it from the
 * Models tab. [parametersBillions] is the model's size as its name states it.
 */
data class CandidateModel(
    val name: String,
    val repo: String,
    val file: String,
    val fileBytes: Long,
    val parametersBillions: Double,
)

/** A model already on the phone, with what its GGUF header says about its memory use. */
data class InstalledModel(
    val id: String,
    val fileBytes: Long,
    val parametersBillions: Double?,
    val layers: Int?,
    val embeddingLength: Int?,
)

enum class Speed(val label: String) {
    COMFORTABLE("comfortable"),
    USABLE("usable"),
    SLOW("slow"),
}

enum class Basis(val label: String) {
    MEASURED("measured on this phone"),
    ESTIMATED("estimated from the file size"),
}

/** One model with what the phone should give for it. Speeds are tokens written per second. */
data class Suggestion(
    val name: String,
    val repo: String?,
    val file: String?,
    val installed: Boolean,
    val parametersBillions: Double?,
    val decodeTokensPerSecond: Double,
    val basis: Basis,
    val requiredRamBytes: Long,
    val fitsRam: Boolean,
    val speed: Speed,
)

/**
 * The advice for this phone. [best] is the largest model that fits in memory and runs at a usable speed. [fastest] is the
 * quickest model of at least one billion parameters that fits. Both are null when nothing qualifies.
 */
data class Advice(
    val bandwidthBytesPerSecond: Double,
    val ramBudgetBytes: Long,
    val best: Suggestion?,
    val fastest: Suggestion?,
    val all: List<Suggestion>,
)

/**
 * Turns a benchmark and the phone's memory into model suggestions with speed estimates.
 *
 * Decoding a token reads the whole model once, so tokens per second is about the phone's memory bandwidth divided by the file
 * size. The bandwidth is taken from the models measured on the phone, or a default when none were measured. Estimates for
 * models not yet measured use it; a measured model uses its own measured speed.
 */
object ModelAdvisor {
    /** Effective bandwidth in bytes per second, from LFM2 measurements on the test phone. Used when nothing is measured. */
    const val DEFAULT_BANDWIDTH = 10.5e9

    /** Memory kept free for Android and other apps, on top of the 30% held back. */
    const val RESERVED_BYTES = 2_000_000_000L

    /** Memory the runtime needs beyond the weights and the context cache. */
    const val OVERHEAD_BYTES = 400L * 1024 * 1024

    const val COMFORTABLE_TOKENS_PER_SECOND = 8.0
    const val USABLE_TOKENS_PER_SECOND = 4.0

    /** The memory a model may use on this phone: 70% of RAM, and never more than RAM minus the reserve. */
    fun ramBudget(totalRamBytes: Long): Long =
        minOf((totalRamBytes * 0.7).toLong(), totalRamBytes - RESERVED_BYTES).coerceAtLeast(0L)

    /** Weights plus the key and value cache for [contextTokens], plus the runtime's overhead. */
    fun requiredRam(fileBytes: Long, layers: Int?, embeddingLength: Int?, contextTokens: Int): Long {
        val cache = if (layers != null && embeddingLength != null) {
            // Keys and values, 2 bytes each, per layer, per context token. Grouped-query attention needs less, so this is generous.
            2L * layers * embeddingLength * 2L * contextTokens
        } else {
            fileBytes / 5
        }
        return fileBytes + cache + OVERHEAD_BYTES
    }

    /** The bandwidth to estimate with: the median of the measured models, or [DEFAULT_BANDWIDTH], scaled by sustained speed. */
    fun bandwidth(runs: List<ModelRun>, stress: StressResult?): Double {
        val measured = runs.filter { it.decodeTokensPerSecond > 0.0 }
            .map { it.decodeTokensPerSecond * it.fileBytes }
            .sorted()
        val base = if (measured.isEmpty()) DEFAULT_BANDWIDTH else measured[measured.size / 2]
        return base * (stress?.sustainedRatio ?: 1.0)
    }

    fun speedFor(tokensPerSecond: Double): Speed = when {
        tokensPerSecond >= COMFORTABLE_TOKENS_PER_SECOND -> Speed.COMFORTABLE
        tokensPerSecond >= USABLE_TOKENS_PER_SECOND -> Speed.USABLE
        else -> Speed.SLOW
    }

    fun advise(
        totalRamBytes: Long,
        contextTokens: Int,
        installed: List<InstalledModel>,
        runs: List<ModelRun>,
        stress: StressResult?,
        candidates: List<CandidateModel>,
    ): Advice {
        val budget = ramBudget(totalRamBytes)
        val bandwidth = bandwidth(runs, stress)
        val measuredById = runs.filter { it.decodeTokensPerSecond > 0.0 }.associateBy { it.modelId }

        val fromInstalled = installed.map { model ->
            val measured = measuredById[model.id]
            val tokens = measured?.decodeTokensPerSecond ?: (bandwidth / model.fileBytes)
            val required = requiredRam(model.fileBytes, model.layers, model.embeddingLength, contextTokens)
            Suggestion(
                name = model.id,
                repo = null,
                file = null,
                installed = true,
                parametersBillions = model.parametersBillions,
                decodeTokensPerSecond = tokens,
                basis = if (measured != null) Basis.MEASURED else Basis.ESTIMATED,
                requiredRamBytes = required,
                fitsRam = required <= budget,
                speed = speedFor(tokens),
            )
        }
        val fromCandidates = candidates.map { candidate ->
            val tokens = bandwidth / candidate.fileBytes
            val required = requiredRam(candidate.fileBytes, null, null, contextTokens)
            Suggestion(
                name = candidate.name,
                repo = candidate.repo,
                file = candidate.file,
                installed = false,
                parametersBillions = candidate.parametersBillions,
                decodeTokensPerSecond = tokens,
                basis = Basis.ESTIMATED,
                requiredRamBytes = required,
                fitsRam = required <= budget,
                speed = speedFor(tokens),
            )
        }
        val all = (fromInstalled + fromCandidates).sortedWith(
            compareByDescending<Suggestion> { it.fitsRam }
                .thenByDescending { it.parametersBillions ?: 0.0 }
                .thenByDescending { it.decodeTokensPerSecond },
        )
        val best = all.filter { it.fitsRam && it.decodeTokensPerSecond >= USABLE_TOKENS_PER_SECOND }
            .maxWithOrNull(compareBy<Suggestion> { it.parametersBillions ?: 0.0 }.thenBy { it.decodeTokensPerSecond })
        val fastest = all.filter { it.fitsRam && (it.parametersBillions ?: 0.0) >= 1.0 }
            .maxByOrNull { it.decodeTokensPerSecond }
        return Advice(bandwidth, budget, best, fastest, all)
    }
}
