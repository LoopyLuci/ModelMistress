package com.modelmistress.runtime

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.modelmistress.core.models.HfFile
import com.modelmistress.core.models.HfSpec
import com.modelmistress.core.models.HuggingFaceFiles
import com.modelmistress.core.models.RepoFiles
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Finds a GGUF model in a Hugging Face repo, downloads it into the library, and keeps the optional token for gated repos.
 * [onInstalled] runs when a download lands, so the model list can refresh.
 */
@Composable
fun HuggingFacePanel(runtime: ModelRuntime, onInstalled: () -> Unit, modifier: Modifier = Modifier) {
    val scope = rememberCoroutineScope()
    val download by runtime.downloads.states.collectAsState()
    var repoText by remember { mutableStateOf("") }
    var tokenText by remember { mutableStateOf("") }
    var tokenSaved by remember { mutableStateOf(runtime.settings.hfToken != null) }
    var listing by remember { mutableStateOf<RepoFiles?>(null) }
    var listedSpec by remember { mutableStateOf<HfSpec?>(null) }
    var message by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }

    LaunchedEffect(download.phase) {
        if (download.phase == DownloadPhase.DONE) onInstalled()
    }

    fun findFiles() {
        val parsed = try {
            HfSpec.parse(repoText)
        } catch (error: IllegalArgumentException) {
            message = error.message
            return
        }
        busy = true
        message = "Reading the repo's files."
        scope.launch {
            val result = runCatching { withContext(Dispatchers.IO) { runtime.downloads.listFiles(parsed) } }
            busy = false
            result.onSuccess { repo ->
                listing = repo
                listedSpec = parsed
                message = if (HuggingFaceFiles.candidates(repo.files).isEmpty()) {
                    "This repo has no single-file GGUF model this app can download."
                } else {
                    null
                }
            }.onFailure { error ->
                listing = null
                message = error.message ?: "The repo could not be read."
            }
        }
    }

    fun startDownload(file: HfFile) {
        val repo = listing ?: return
        val spec = listedSpec ?: return
        message = if (runtime.downloads.start(spec, repo.revision, file)) {
            null
        } else {
            "Another download is running. Wait for it, or stop it first."
        }
    }

    fun saveToken() {
        val trimmed = tokenText.trim()
        runtime.settings.hfToken = trimmed.takeIf { it.isNotEmpty() }
        tokenSaved = runtime.settings.hfToken != null
        tokenText = ""
        message = if (tokenSaved) "Token saved on this device." else "Token removed."
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Get a model from Hugging Face", style = MaterialTheme.typography.titleMedium)
        Text(
            "Enter a repo such as unsloth/Qwen3-8B-GGUF, or with a quantisation such as org/repo:Q4_K_M.",
            style = MaterialTheme.typography.bodySmall,
        )
        OutlinedTextField(
            value = repoText,
            onValueChange = { repoText = it },
            placeholder = { Text("org/repo") },
            singleLine = true,
            enabled = !busy,
            modifier = Modifier.fillMaxWidth(),
        )
        Button(onClick = ::findFiles, enabled = !busy && repoText.isNotBlank()) { Text("Find models") }

        listing?.let { repo ->
            HuggingFaceFiles.candidates(repo.files).forEach { file ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            file.path,
                            style = MaterialTheme.typography.labelLarge,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            "${file.quant ?: "quantisation not named"} · ${formatSize(file.sizeBytes)}",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    TextButton(
                        onClick = { startDownload(file) },
                        enabled = download.phase != DownloadPhase.DOWNLOADING,
                    ) { Text("Download") }
                }
            }
        }

        if (download.phase == DownloadPhase.DOWNLOADING) {
            val fraction = if (download.total > 0) {
                (download.done.toFloat() / download.total).coerceIn(0f, 1f)
            } else {
                0f
            }
            LinearProgressIndicator(progress = { fraction }, modifier = Modifier.fillMaxWidth())
            Text(
                "${download.file}: ${formatSize(download.done)} of ${formatSize(download.total)}",
                style = MaterialTheme.typography.bodySmall,
            )
            OutlinedButton(onClick = runtime.downloads::stop) { Text("Stop") }
        }
        if (download.phase == DownloadPhase.STOPPED || download.phase == DownloadPhase.FAILED) {
            // the part already downloaded is kept, so this resumes it
            OutlinedButton(onClick = {
                message = if (runtime.downloads.restart()) null else "The download cannot start again from here."
            }) { Text("Restart") }
        }
        (message ?: download.message.takeIf { download.phase != DownloadPhase.DOWNLOADING })?.let {
            Text(it, style = MaterialTheme.typography.bodySmall)
        }

        Text(
            "Token for gated or private repos only. ${if (tokenSaved) "A token is saved." else "None saved."}",
            style = MaterialTheme.typography.bodySmall,
        )
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = tokenText,
                onValueChange = { tokenText = it },
                placeholder = { Text("hf_…") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = ::saveToken, enabled = tokenText.isNotBlank()) { Text("Save") }
            if (tokenSaved) {
                TextButton(onClick = {
                    runtime.settings.hfToken = null
                    tokenSaved = false
                    message = "Token removed."
                }) { Text("Remove") }
            }
        }
    }
}
