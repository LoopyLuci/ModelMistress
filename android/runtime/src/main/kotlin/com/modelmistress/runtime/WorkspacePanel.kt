package com.modelmistress.runtime

import android.content.ContentResolver
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
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
import com.modelmistress.core.tools.Workspace
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** The text files the chat's tools may read. Only the files added here are visible to the model. */
@Composable
fun WorkspacePanel(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val workspace = runtime.workspace
    var files by remember { mutableStateOf<List<File>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }

    fun refresh() {
        scope.launch { files = withContext(Dispatchers.IO) { workspace.files() } }
    }

    LaunchedEffect(Unit) { refresh() }

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) {
            scope.launch {
                busy = true
                message = try {
                    val stored = withContext(Dispatchers.IO) { addFile(context.contentResolver, uri, workspace) }
                    "Added ${stored.name}."
                } catch (error: Exception) {
                    error.message ?: "The file could not be added."
                }
                busy = false
                refresh()
            }
        }
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Files for the chat's tools", style = MaterialTheme.typography.titleMedium)
        Text(
            "Text files the model can search and read. Only the files you add here are visible to it. " +
                "Secret files, such as .env or key files, are refused.",
            style = MaterialTheme.typography.bodySmall,
        )
        Button(onClick = { picker.launch(arrayOf("text/*", "application/json")) }, enabled = !busy) {
            Text("Add a text file")
        }
        if (files.isEmpty()) {
            Text("No files yet.", style = MaterialTheme.typography.bodySmall)
        }
        files.forEach { file ->
            Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(file.name, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                TextButton(onClick = {
                    scope.launch {
                        withContext(Dispatchers.IO) { workspace.remove(file.name) }
                        message = "Removed ${file.name}."
                        refresh()
                    }
                }) { Text("Remove") }
            }
        }
        message?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
    }
}

/** Copies a picked text file into the workspace under a safe name. Runs off the main thread. */
private fun addFile(resolver: ContentResolver, uri: Uri, workspace: Workspace): File {
    val name = Workspace.safeName(displayName(resolver, uri))
    val input = resolver.openInputStream(uri) ?: throw IOException("$name could not be opened.")
    val bytes = input.use { stream ->
        val out = ByteArrayOutputStream()
        val buffer = ByteArray(8192)
        while (true) {
            val read = stream.read(buffer)
            if (read < 0) break
            out.write(buffer, 0, read)
            if (out.size() > Workspace.MAX_STORE_BYTES) throw IOException("$name is larger than 2 MB.")
        }
        out.toByteArray()
    }
    return workspace.store(name, bytes)
}

private fun displayName(resolver: ContentResolver, uri: Uri): String =
    resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
        if (cursor.moveToFirst()) cursor.getString(0) else null
    } ?: uri.lastPathSegment?.substringAfterLast('/') ?: "file.txt"
