package com.modelmistress.runtime

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.util.Locale

/** Lists the models on this device, imports GGUF and LiteRT-LM files, sets the default and deletes. */
@Composable
fun ModelsPanel(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var models by remember { mutableStateOf<List<LocalModel>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var defaultId by remember { mutableStateOf(runtime.settings.defaultModelId) }

    fun refresh() {
        scope.launch {
            models = withContext(Dispatchers.IO) { runtime.library.list() }
        }
    }

    LaunchedEffect(Unit) { refresh() }

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) {
            scope.launch {
                busy = true
                message = null
                message = try {
                    val imported = withContext(Dispatchers.IO) {
                        runtime.library.import(context.contentResolver, uri)
                    }
                    "Imported ${imported.id}."
                } catch (error: Exception) {
                    error.message ?: "The model could not be imported."
                }
                busy = false
                refresh()
            }
        }
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = { picker.launch(arrayOf("*/*")) }, enabled = !busy) {
                Text("Import model")
            }
            if (busy) CircularProgressIndicator()
        }
        message?.let { Text(it, style = MaterialTheme.typography.bodySmall) }

        if (models.isEmpty()) {
            Text(
                "No models yet. Import a GGUF or .litertlm file, for example one you downloaded to Downloads.",
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        models.forEach { model ->
            Column {
                val isDefault = model.id == defaultId
                Text(
                    if (isDefault) "${model.id} (default)" else model.id,
                    style = MaterialTheme.typography.labelLarge,
                )
                Text(
                    "${model.summary} · ${formatSize(model.sizeBytes)}" +
                        (model.info?.contextLength?.let { " · context $it" } ?: ""),
                    style = MaterialTheme.typography.bodySmall,
                )
                Row {
                    TextButton(onClick = {
                        runtime.settings.defaultModelId = model.id
                        defaultId = model.id
                    }) { Text("Use by default") }
                    TextButton(onClick = {
                        runtime.library.delete(model.id)
                        refresh()
                    }) { Text("Delete") }
                }
            }
        }
        HorizontalDivider()
        HuggingFacePanel(runtime, onInstalled = { refresh() })
        HorizontalDivider()
        ImageModelsPanel(runtime)
        HorizontalDivider()
        BenchmarkPanel(runtime)
        HorizontalDivider()
        MistyPanel(runtime)
        HorizontalDivider()
        WorkspacePanel(runtime)
        HorizontalDivider()
        DesktopsPanel(runtime)
    }
}

internal fun formatSize(bytes: Long): String = when {
    bytes >= (1L shl 30) -> String.format(Locale.US, "%.1f GB", bytes / (1L shl 30).toDouble())
    bytes >= (1L shl 20) -> String.format(Locale.US, "%.0f MB", bytes / (1L shl 20).toDouble())
    else -> "$bytes B"
}
