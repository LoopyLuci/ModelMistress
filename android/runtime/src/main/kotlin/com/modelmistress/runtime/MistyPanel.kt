package com.modelmistress.runtime

import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.modelmistress.cactus.CactusNeedle
import com.modelmistress.cactus.CactusNeedleModel
import com.modelmistress.core.misty.FeedbackLabel
import com.modelmistress.core.misty.MistyTraining
import com.modelmistress.core.models.HuggingFaceClient
import com.modelmistress.core.tools.FileWrites
import com.modelmistress.core.tools.ToolUse
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

/** Writes the training examples to a file in the app's cache, one JSON object per line. Null when there are none. */
private fun writeTraining(context: Context, runtime: ModelRuntime): File? {
    val lines = MistyTraining.lines(runtime.feedback.examples())
    if (lines.isEmpty()) return null
    val folder = File(context.cacheDir, "exports").apply { mkdirs() }
    return File(folder, "misty-training.jsonl").apply { writeText(lines.joinToString("\n", postfix = "\n")) }
}

/**
 * Misty's switches, the tool and file-write modes, and Needle 3's download, on the Models screen. Misty is off until the owner
 * turns it on. Needle is fetched from its pinned revision only when the owner asks for it.
 */
@Composable
fun MistyPanel(runtime: ModelRuntime, onExportTraining: ((File) -> Unit)? = null, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val settings = runtime.settings
    val scope = rememberCoroutineScope()
    var misty by remember { mutableStateOf(settings.mistyEnabled) }
    var reflex by remember { mutableStateOf(settings.mistyReflex) }
    var toolUse by remember { mutableStateOf(settings.toolUse) }
    var fileWrites by remember { mutableStateOf(settings.fileWrites) }
    var installed by remember { mutableStateOf<Boolean?>(null) }
    var ratings by remember { mutableStateOf<Int?>(null) }
    var training by remember { mutableStateOf<Int?>(null) }
    var corrected by remember { mutableStateOf<Int?>(null) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    val progress = remember { MutableStateFlow<Pair<Long, Long>?>(null) }
    val current by progress.collectAsState()
    val runtimeAvailable = remember { CactusNeedle.runtimeAvailable() }

    // Hashing the file is slow, so the check runs off the main thread.
    LaunchedEffect(Unit) {
        installed = withContext(Dispatchers.IO) { runtime.needle.isInstalled() }
        val all = withContext(Dispatchers.IO) { runtime.feedback.examples() }
        ratings = all.size
        training = MistyTraining.examples(all).size
        corrected = all.count { it.label == FeedbackLabel.CORRECTED }
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text("Misty and the chat's tools", style = MaterialTheme.typography.titleMedium)
        Text(
            "A question gets only the tools it needs: the time, the calculator, files to read, create and edit, the " +
                "device status and the models list. A greeting gets none. Each chat works in its own sandbox, a folder " +
                "on this phone that other apps cannot see. Rate a reply under the chat to teach Misty which tools a kind " +
                "of question needs.",
            style = MaterialTheme.typography.bodySmall,
        )
        SwitchRow("Use Misty for chats", misty) {
            misty = it
            settings.mistyEnabled = it
        }
        SwitchRow("Let Needle try obvious tool calls first", reflex) {
            reflex = it
            settings.mistyReflex = it
        }

        ModeRow(
            label = "Tool use",
            selected = toolUse,
            options = listOf(ToolUse.OFF to "Off", ToolUse.ON to "On", ToolUse.FULL_AUTO to "Full auto"),
            onSelect = {
                toolUse = it
                settings.toolUse = it
            },
        )
        Text(
            when (toolUse) {
                ToolUse.OFF -> "No tools. Every reply is plain chat."
                ToolUse.ON -> "Reading and checking run at once. A file change asks you first, each time."
                ToolUse.FULL_AUTO -> "Every tool runs at once. File changes run without asking when File writes is Full auto too."
            },
            style = MaterialTheme.typography.bodySmall,
        )
        ModeRow(
            label = "File writes",
            selected = fileWrites,
            options = listOf(FileWrites.OFF to "Off", FileWrites.ON to "On", FileWrites.FULL_AUTO to "Full auto"),
            onSelect = {
                fileWrites = it
                settings.fileWrites = it
            },
        )
        Text(
            when (fileWrites) {
                FileWrites.OFF -> "Files are never created, changed or replaced."
                FileWrites.ON -> "Each file the chat creates or changes asks you first."
                FileWrites.FULL_AUTO -> "File changes run at once, but only while Tool use is Full auto."
            },
            style = MaterialTheme.typography.bodySmall,
        )

        Text(
            "Ratings kept: ${ratings ?: "…"}. Training examples: ${training ?: "…"}, of which ${corrected ?: "…"} corrected by voice.",
            style = MaterialTheme.typography.bodySmall,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(
                enabled = !busy && (ratings ?: 0) > 0,
                onClick = {
                    scope.launch {
                        withContext(Dispatchers.IO) { runtime.feedback.clear() }
                        ratings = 0
                        training = 0
                        corrected = 0
                    }
                },
            ) {
                Text("Forget ratings")
            }
            // The export needs the app's share sheet, so the router, which has none, does not show it.
            if (onExportTraining != null) {
                TextButton(
                    enabled = !busy && (training ?: 0) > 0,
                    onClick = {
                        scope.launch {
                            val file = withContext(Dispatchers.IO) { writeTraining(context, runtime) }
                            file?.let(onExportTraining)
                        }
                    },
                ) {
                    Text("Export training data")
                }
            }
        }

        Text("Needle 3, the reflex model (${formatSize(CactusNeedleModel.SIZE_BYTES)})", style = MaterialTheme.typography.labelLarge)
        Text(
            when {
                !runtimeAvailable -> "The Cactus runtime is not available on this Android ABI, so Needle cannot run."
                installed == null -> "Checking the model file…"
                installed == true -> "Installed, and its checksum matches."
                else -> "Not installed. Misty works without it; Needle only adds a first try at obvious tool calls."
            },
            style = MaterialTheme.typography.bodySmall,
        )
        current?.let { (done, total) ->
            val percent = if (total > 0) (done * 100 / total) else 0
            Text("Downloading: $percent%", style = MaterialTheme.typography.bodySmall)
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (runtimeAvailable && installed == false) {
                Button(
                    enabled = !busy,
                    onClick = {
                        scope.launch {
                            busy = true
                            message = null
                            message = try {
                                withContext(Dispatchers.IO) {
                                    runtime.needle.download(
                                        client = HuggingFaceClient(settings.hfToken),
                                        progress = { done, total -> progress.value = done to total },
                                        keepGoing = { true },
                                    )
                                }
                                "Needle 3 is installed."
                            } catch (error: Exception) {
                                error.message ?: "Needle 3 could not be downloaded."
                            }
                            progress.value = null
                            installed = withContext(Dispatchers.IO) { runtime.needle.isInstalled() }
                            busy = false
                        }
                    },
                ) {
                    Text("Download Needle 3")
                }
            }
            if (installed == true) {
                TextButton(
                    enabled = !busy,
                    onClick = {
                        scope.launch {
                            withContext(Dispatchers.IO) { runtime.needle.delete() }
                            installed = false
                        }
                    },
                ) {
                    Text("Delete")
                }
            }
            if (busy) CircularProgressIndicator()
        }
        message?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
    }
}

/** One setting with its choices as buttons. The chosen one is filled in. */
@Composable
private fun <T> ModeRow(label: String, selected: T, options: List<Pair<T, String>>, onSelect: (T) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(label, style = MaterialTheme.typography.labelLarge)
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            options.forEach { (value, text) ->
                if (value == selected) {
                    Button(onClick = { onSelect(value) }) { Text(text) }
                } else {
                    OutlinedButton(onClick = { onSelect(value) }) { Text(text) }
                }
            }
        }
    }
}

@Composable
private fun SwitchRow(label: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(label, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = onChange)
    }
}
