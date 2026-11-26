package com.modelmistress.runtime

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.unit.dp
import com.modelmistress.cactus.CactusNeedle
import com.modelmistress.cactus.CactusNeedleModel
import com.modelmistress.core.models.HuggingFaceClient
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Misty's switches and Needle 3's download, on the Models screen. Misty is off until the owner turns it on. Needle is
 * fetched from its pinned revision only when the owner asks for it.
 */
@Composable
fun MistyPanel(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val settings = runtime.settings
    val scope = rememberCoroutineScope()
    var misty by remember { mutableStateOf(settings.mistyEnabled) }
    var reflex by remember { mutableStateOf(settings.mistyReflex) }
    var askWrite by remember { mutableStateOf(settings.mistyAskBeforeWrite) }
    var installed by remember { mutableStateOf<Boolean?>(null) }
    var ratings by remember { mutableStateOf<Int?>(null) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    val progress = remember { MutableStateFlow<Pair<Long, Long>?>(null) }
    val current by progress.collectAsState()
    val runtimeAvailable = remember { CactusNeedle.runtimeAvailable() }

    // Hashing the file is slow, so the check runs off the main thread.
    LaunchedEffect(Unit) {
        installed = withContext(Dispatchers.IO) { runtime.needle.isInstalled() }
        ratings = withContext(Dispatchers.IO) { runtime.feedback.examples().size }
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text("Misty", style = MaterialTheme.typography.titleMedium)
        Text(
            "A question gets only the tools it needs: the time, the calculator, files to read, create and edit, the " +
                "device status and the models list. A greeting gets none. Files change only when you allow each write " +
                "or edit. Rate a reply under the chat to teach Misty which tools a kind of question needs.",
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
        SwitchRow("Ask before Misty writes a file (off refuses writes)", askWrite) {
            askWrite = it
            settings.mistyAskBeforeWrite = it
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                "Ratings kept: ${ratings ?: "…"}",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.weight(1f),
            )
            TextButton(
                enabled = !busy && (ratings ?: 0) > 0,
                onClick = {
                    scope.launch {
                        withContext(Dispatchers.IO) { runtime.feedback.clear() }
                        ratings = 0
                    }
                },
            ) {
                Text("Forget ratings")
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
