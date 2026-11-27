package com.modelmistress.runtime

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
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

/**
 * Lists the Stable Diffusion checkpoints on this device, imports them and deletes them. The server's image route makes
 * images with them.
 */
@Composable
fun ImageModelsPanel(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var models by remember { mutableStateOf<List<LocalImageModel>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }

    fun refresh() {
        scope.launch {
            models = withContext(Dispatchers.IO) { runtime.imageLibrary.list() }
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
                        runtime.imageLibrary.import(context.contentResolver, uri)
                    }
                    "Imported ${imported.id}."
                } catch (error: Exception) {
                    error.message ?: "The image model could not be imported."
                }
                busy = false
                refresh()
            }
        }
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text("Image models", style = MaterialTheme.typography.titleMedium)
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = { picker.launch(arrayOf("*/*")) }, enabled = !busy) {
                Text("Import image model")
            }
            if (busy) CircularProgressIndicator()
        }
        message?.let { Text(it, style = MaterialTheme.typography.bodySmall) }

        if (models.isEmpty()) {
            Text(
                "No image models. Import a Stable Diffusion .safetensors or .ckpt checkpoint. A generation takes minutes " +
                    "on a phone's CPU.",
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        models.forEach { model ->
            Column {
                Text(model.id, style = MaterialTheme.typography.labelLarge)
                Text(
                    "Stable Diffusion checkpoint · ${formatSize(model.sizeBytes)}",
                    style = MaterialTheme.typography.bodySmall,
                )
                TextButton(onClick = {
                    scope.launch {
                        withContext(Dispatchers.IO) {
                            runtime.imageHub.unloadAll()
                            runtime.imageLibrary.delete(model.id)
                        }
                        refresh()
                    }
                }) { Text("Delete") }
            }
        }
    }
}
