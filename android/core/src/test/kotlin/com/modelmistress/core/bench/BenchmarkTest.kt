package com.modelmistress.core.bench

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class BenchmarkTest {
    private val gb = 1_000_000_000L
    private val device = DeviceProfile("Redmi Note 12 Pro 5G", 34, 8, 7_700_000_000L, "arm64-v8a", true)

    @Test
    fun decodeSpeedCountsTheTokensAfterTheFirst() {
        // 65 tokens, the first after 1000 ms, the last at 9000 ms: 64 tokens in 8000 ms is 8 tokens a second.
        val run = ModelRun("m", gb, promptTokens = 20, generatedTokens = 65, timeToFirstTokenMs = 1000, totalMs = 9000)
        assertEquals(8.0, run.decodeTokensPerSecond, 0.0001)
    }

    @Test
    fun aRunWithTooLittleOutputHasNoDecodeSpeed() {
        val run = ModelRun("m", gb, promptTokens = 20, generatedTokens = 1, timeToFirstTokenMs = 1000, totalMs = 1000)
        assertEquals(0.0, run.decodeTokensPerSecond, 0.0)
    }

    @Test
    fun promptSpeedIsPromptTokensOverTheFirstTokenTime() {
        val run = ModelRun("m", gb, promptTokens = 200, generatedTokens = 10, timeToFirstTokenMs = 2000, totalMs = 4000)
        assertEquals(100.0, run.promptTokensPerSecond, 0.0001)
    }

    @Test
    fun sustainedRatioIsTheLateSpeedShareOfTheEarlyOne() {
        assertEquals(0.6, stress(first = 100.0, last = 60.0).sustainedRatio, 0.0001)
        assertEquals(1.0, stress(first = 0.0, last = 60.0).sustainedRatio, 0.0001)
        assertEquals(0.1, stress(first = 100.0, last = 1.0).sustainedRatio, 0.0001)
    }

    @Test
    fun aReportSurvivesTheStoreAndComesBackEqual() {
        val report = BenchmarkReport(
            createdAt = 1_795_700_000_000L,
            device = device,
            stress = stress(first = 120.0, last = 90.0),
            runs = listOf(ModelRun("LFM2-2.6B-Q4_0", 1_414_000_000L, 40, 64, 3400, 11_000)),
        )
        val decoded = BenchmarkCodec.decode(BenchmarkCodec.encode(report))
        assertEquals(report, decoded)
    }

    @Test
    fun aReportWithoutAStressTestSurvivesTheStoreToo() {
        val report = BenchmarkReport(createdAt = 5L, device = device, stress = null, runs = emptyList())
        assertEquals(report, BenchmarkCodec.decode(BenchmarkCodec.encode(report)))
    }

    @Test
    fun textThatIsNotAReportIsRefused() {
        assertNull(BenchmarkCodec.decode("not json"))
        assertNull(BenchmarkCodec.decode("""{"createdAt": 1}"""))
    }

    @Test
    fun theMemoryBudgetKeepsRoomForAndroid() {
        // A 7.7 GB phone: 70% is 5.39 GB, which is less than 7.7 GB minus the 2 GB reserve.
        assertEquals((7_700_000_000L * 0.7).toLong(), ModelAdvisor.ramBudget(7_700_000_000L))
        // A 3 GB phone: 70% would be 2.1 GB, but the reserve leaves only 1 GB.
        assertEquals(1_000_000_000L, ModelAdvisor.ramBudget(3_000_000_000L))
        assertEquals(0L, ModelAdvisor.ramBudget(1_000_000_000L))
    }

    @Test
    fun theRequiredMemoryCountsTheContextCache() {
        // 24 layers, embedding 2048, 2048 tokens: keys and values are 2 * 24 * 2048 * 2 * 2048 bytes.
        val required = ModelAdvisor.requiredRam(1 * gb, layers = 24, embeddingLength = 2048, contextTokens = 2048)
        assertEquals(1 * gb + 402_653_184L + ModelAdvisor.OVERHEAD_BYTES, required)
    }

    @Test
    fun withoutAHeaderTheCacheIsAFifthOfTheFile() {
        val required = ModelAdvisor.requiredRam(1 * gb, layers = null, embeddingLength = null, contextTokens = 2048)
        assertEquals(1 * gb + gb / 5 + ModelAdvisor.OVERHEAD_BYTES, required)
    }

    @Test
    fun aCandidateIsEstimatedFromTheDefaultBandwidth() {
        val advice = advise(candidates = listOf(candidate("one", 1 * gb, 1.0)))
        val suggestion = advice.all.single { it.name == "one" }
        assertEquals(ModelAdvisor.DEFAULT_BANDWIDTH / gb, suggestion.decodeTokensPerSecond, 0.0001)
        assertEquals(Basis.ESTIMATED, suggestion.basis)
        assertFalse(suggestion.installed)
    }

    @Test
    fun aMeasuredModelUsesItsOwnSpeed() {
        val installed = InstalledModel("LFM2-1.2B", 1 * gb, 1.2, layers = null, embeddingLength = null)
        val runs = listOf(ModelRun("LFM2-1.2B", 1 * gb, 20, 65, 1000, 9000))
        val advice = advise(installed = listOf(installed), runs = runs)
        val suggestion = advice.all.single { it.name == "LFM2-1.2B" }
        assertEquals(8.0, suggestion.decodeTokensPerSecond, 0.0001)
        assertEquals(Basis.MEASURED, suggestion.basis)
        assertTrue(suggestion.installed)
    }

    @Test
    fun theBandwidthIsTheMedianOfTheMeasuredModels() {
        val runs = listOf(
            ModelRun("a", 1 * gb, 1, 11, 0, 10_000),
            ModelRun("b", 2 * gb, 1, 11, 0, 20_000),
            ModelRun("c", 4 * gb, 1, 11, 0, 40_000),
        )
        // Each run reads its whole file once per token: 1 GB at 1 token a second, and so on. The median is 1 GB a second.
        assertEquals(1.0 * gb, ModelAdvisor.bandwidth(runs, stress = null), 1_000_000.0)
    }

    @Test
    fun thermalSlowdownLowersTheEstimate() {
        val slow = stress(first = 100.0, last = 50.0)
        val advice = advise(candidates = listOf(candidate("one", 1 * gb, 1.0)), stress = slow)
        assertEquals(ModelAdvisor.DEFAULT_BANDWIDTH * 0.5 / gb, advice.all.single().decodeTokensPerSecond, 0.0001)
    }

    @Test
    fun theBestModelIsTheLargestThatFitsAtAUsableSpeed() {
        val advice = advise(
            totalRamBytes = 8 * gb,
            candidates = listOf(
                candidate("small", 1 * gb, 1.7),
                candidate("medium", 2 * gb, 3.0),
                candidate("huge", 9 * gb, 8.0),
            ),
        )
        assertEquals("medium", advice.best?.name)
        assertFalse(advice.all.single { it.name == "huge" }.fitsRam)
    }

    @Test
    fun aModelBelowTheUsableSpeedIsNotTheBest() {
        // 3.5 GB at 10.5 GB a second is 3 tokens a second, below the usable 4: the 1.7B model is the best.
        val advice = advise(
            totalRamBytes = 16 * gb,
            candidates = listOf(candidate("slow big", 3_500_000_000L, 4.0), candidate("small", 1 * gb, 1.7)),
        )
        assertEquals("small", advice.best?.name)
    }

    @Test
    fun theFastestModelHasAtLeastABillionParameters() {
        val advice = advise(
            totalRamBytes = 8 * gb,
            candidates = listOf(candidate("tiny", 300_000_000L, 0.5), candidate("one", 1 * gb, 1.2)),
        )
        assertEquals("one", advice.fastest?.name)
    }

    @Test
    fun suggestionsThatFitComeFirst() {
        // A 3 GB phone has a 1 GB budget. A 0.4 GB model needs 0.88 GB with its cache and overhead, so it fits.
        val advice = advise(
            totalRamBytes = 3 * gb,
            candidates = listOf(candidate("too big", 4 * gb, 9.0), candidate("fits", 400_000_000L, 0.5)),
        )
        assertEquals(listOf("fits", "too big"), advice.all.map { it.name })
        assertEquals("fits", advice.best?.name)
        assertTrue(advice.best?.fitsRam == true)
    }

    private fun stress(first: Double, last: Double) = StressResult(
        threads = 8,
        cpuOpsPerSecond = first,
        memoryMegabytesPerSecond = 5000.0,
        durationSeconds = 20.0,
        firstWindowOps = first,
        lastWindowOps = last,
        batteryTempStartC = 30.0,
        batteryTempEndC = 36.5,
        thermalStatus = 0,
    )

    private fun candidate(name: String, bytes: Long, params: Double) =
        CandidateModel(name, "org/$name", "$name.gguf", bytes, params)

    private fun advise(
        totalRamBytes: Long = 8 * gb,
        installed: List<InstalledModel> = emptyList(),
        runs: List<ModelRun> = emptyList(),
        stress: StressResult? = null,
        candidates: List<CandidateModel> = emptyList(),
    ) = ModelAdvisor.advise(totalRamBytes, 2048, installed, runs, stress, candidates)
}
