package com.modelmistress.runtime

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.modelmistress.core.bench.Advice
import com.modelmistress.core.bench.BenchmarkReport
import com.modelmistress.core.bench.CandidateCatalog
import com.modelmistress.core.bench.DeviceProfile
import com.modelmistress.core.bench.InstalledModel
import com.modelmistress.core.bench.ModelAdvisor
import com.modelmistress.core.bench.StressResult
import com.modelmistress.core.bench.Suggestion
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.util.Locale

/**
 * The device benchmark: the phone's profile, a stress test, each installed model's speed with a fixed prompt, and the models
 * that suit this phone with speed estimates. The results are kept on the device until the next run.
 */
@Composable
fun BenchmarkPanel(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val device = remember { PhoneProfile.read(context) }
    var report by remember { mutableStateOf(runtime.benchmarks.load()) }
    var installed by remember { mutableStateOf<List<InstalledModel>>(emptyList()) }
    var installedIds by remember { mutableStateOf<List<String>>(emptyList()) }
    var running by remember { mutableStateOf(false) }
    var progress by remember { mutableStateOf<String?>(null) }
    var failures by remember { mutableStateOf<List<String>>(emptyList()) }

    LaunchedEffect(Unit) {
        val models = withContext(Dispatchers.IO) { runtime.library.list() }
        installed = models.map { model ->
            InstalledModel(
                id = model.id,
                fileBytes = model.sizeBytes,
                parametersBillions = parametersFrom(model.id),
                layers = model.info?.blockCount,
                embeddingLength = model.info?.embeddingLength,
            )
        }
        installedIds = models.map { it.id }
    }

    fun run() {
        if (running) return
        scope.launch {
            running = true
            failures = emptyList()
            val ids = installedIds
            val outcome = withContext(Dispatchers.IO) {
                val status: (String) -> Unit = { text -> scope.launch { progress = text } }
                val stress: StressResult? = runCatching { StressTester(context).run(onProgress = status) }.getOrNull()
                val timed = ModelBench(runtime.hub, runtime.library).run(ids) { text -> status(text) }
                Pair(stress, timed)
            }
            val (stress, timed) = outcome
            val saved = BenchmarkReport(System.currentTimeMillis(), device, stress, timed.runs)
            runCatching { runtime.benchmarks.save(saved) }
            report = saved
            failures = timed.failures
            progress = null
            running = false
        }
    }

    val advice = remember(report, installed) {
        ModelAdvisor.advise(
            totalRamBytes = device.ramBytes,
            contextTokens = runtime.settings.contextTokens,
            installed = installed,
            runs = report?.runs.orEmpty(),
            stress = report?.stress,
            candidates = CandidateCatalog.models,
        )
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Device benchmark", style = MaterialTheme.typography.titleSmall)
        Text(deviceLine(device), style = MaterialTheme.typography.bodySmall)
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = { run() }, enabled = !running && installedIds.isNotEmpty()) {
                Text(if (running) "Running" else "Run benchmark")
            }
            if (running) CircularProgressIndicator()
        }
        if (installedIds.isEmpty()) {
            Text("Import a model first. The benchmark times the models on this phone.", style = MaterialTheme.typography.bodySmall)
        }
        progress?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        Text(
            "The run takes about a minute for the stress test, plus about a minute per model. It keeps the phone busy, so " +
                "charge it and close other apps first.",
            style = MaterialTheme.typography.labelSmall,
        )

        report?.let { saved -> ReportSection(saved, failures) }
        SuggestionSection(advice, report != null)
    }
}

@Composable
private fun ReportSection(report: BenchmarkReport, failures: List<String>) {
    Text("Last run: ${formatTime(report.createdAt)}", style = MaterialTheme.typography.labelLarge)
    report.stress?.let { stress -> StressLines(stress) }
    if (report.runs.isEmpty()) {
        Text("No model was timed yet.", style = MaterialTheme.typography.bodySmall)
    }
    report.runs.forEach { run ->
        Text(
            "${run.modelId}: ${fmt(run.decodeTokensPerSecond)} tokens a second written, " +
                "first token after ${run.timeToFirstTokenMs} ms, reads ${fmt(run.promptTokensPerSecond)} prompt tokens a second.",
            style = MaterialTheme.typography.bodySmall,
        )
    }
    failures.forEach { Text("Not timed: $it", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
}

@Composable
private fun StressLines(stress: StressResult) {
    val kept = (stress.sustainedRatio * 100).toInt()
    val verdict = when {
        stress.sustainedRatio >= 0.9 -> "holds its speed"
        stress.sustainedRatio >= 0.7 -> "slows a little under load"
        else -> "slows down a lot under load"
    }
    Text(
        "Stress: ${stress.threads} cores, memory copy ${fmt(stress.memoryMegabytesPerSecond)} MB a second. " +
            "The phone keeps $kept% of its early speed; it $verdict.",
        style = MaterialTheme.typography.bodySmall,
    )
    val heat = mutableListOf<String>()
    stress.batteryTempStartC?.let { heat += "battery ${fmt(it)} °C before" }
    stress.batteryTempEndC?.let { heat += "${fmt(it)} °C after" }
    stress.thermalStatus?.let { heat += "thermal status ${thermalLabel(it)}" }
    if (heat.isNotEmpty()) {
        Text("Heat: ${heat.joinToString(", ")}.", style = MaterialTheme.typography.labelSmall)
    }
}

@Composable
private fun SuggestionSection(advice: Advice, hasReport: Boolean) {
    Text("Suggested models for this phone", style = MaterialTheme.typography.labelLarge)
    val basis = if (hasReport) "Measured models use their measured speed." else "Run the benchmark to measure your models."
    val bandwidthGb = fmt(advice.bandwidthBytesPerSecond / 1e9)
    Text(
        "Memory budget ${formatSize(advice.ramBudgetBytes)}. Decoding reads a model's whole file for each token, so the " +
            "estimate is about $bandwidthGb GB a second of memory bandwidth divided by the file size. $basis",
        style = MaterialTheme.typography.labelSmall,
    )
    val best = advice.best
    if (best != null) {
        Text("Best balance: ${suggestionLine(best)}", style = MaterialTheme.typography.bodyMedium)
    } else {
        Text("No model fits at a usable speed yet.", style = MaterialTheme.typography.bodySmall)
    }
    val fastest = advice.fastest
    if (fastest != null && fastest.name != best?.name) {
        Text("Fastest: ${suggestionLine(fastest)}", style = MaterialTheme.typography.bodyMedium)
    }
    advice.all.take(MAX_LISTED).forEach { suggestion -> SuggestionRow(suggestion) }
    if (advice.all.size > MAX_LISTED) {
        Text("${advice.all.size - MAX_LISTED} more are listed in the catalog.", style = MaterialTheme.typography.labelSmall)
    }
}

@Composable
private fun SuggestionRow(suggestion: Suggestion) {
    val params = suggestion.parametersBillions?.let { " · ${fmt(it)}B" }.orEmpty()
    val installed = if (suggestion.installed) " · installed" else ""
    val fit = if (suggestion.fitsRam) "." else ", which is more than this phone's budget."
    Column {
        Text(suggestion.name + params + installed, style = MaterialTheme.typography.labelLarge)
        Text(
            "About ${fmt(suggestion.decodeTokensPerSecond)} tokens a second (${suggestion.basis.label}), " +
                "${suggestion.speed.label}. Needs about ${formatSize(suggestion.requiredRamBytes)} of memory$fit",
            style = MaterialTheme.typography.bodySmall,
        )
        if (!suggestion.installed && suggestion.repo != null) {
            Text(
                "Download in the Hugging Face section of the Models tab: ${suggestion.repo} · ${suggestion.file}",
                style = MaterialTheme.typography.labelSmall,
            )
        }
    }
}

private fun suggestionLine(suggestion: Suggestion): String =
    "${suggestion.name} (about ${fmt(suggestion.decodeTokensPerSecond)} tokens a second, ${suggestion.speed.label})"

private fun deviceLine(device: DeviceProfile): String = listOf(
    device.model,
    formatSize(device.ramBytes) + " RAM",
    "${device.cores} cores",
    device.abi,
    if (device.dotProd) "dot-product instructions" else "no dot-product instructions",
).joinToString(" · ")

private fun thermalLabel(status: Int): String = when (status) {
    0 -> "none"
    1 -> "light"
    2 -> "moderate"
    3 -> "severe"
    4 -> "critical"
    5 -> "emergency"
    6 -> "shutdown"
    else -> "unknown"
}

private fun formatTime(millis: Long): String =
    java.text.DateFormat.getDateTimeInstance(java.text.DateFormat.SHORT, java.text.DateFormat.SHORT)
        .format(java.util.Date(millis))

private fun fmt(value: Double): String = String.format(Locale.US, "%.1f", value)

/** The parameter count a model's name states, such as 2.6 from "LFM2-2.6B-Q4_0". Null when the name states none. */
internal fun parametersFrom(id: String): Double? =
    Regex("""(\d+(?:\.\d+)?)B\b""", RegexOption.IGNORE_CASE).find(id)?.groupValues?.get(1)?.toDoubleOrNull()

private const val MAX_LISTED = 8
