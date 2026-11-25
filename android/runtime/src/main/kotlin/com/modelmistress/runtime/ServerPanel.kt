package com.modelmistress.runtime

import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp

/** Starts, stops and configures the local server. Used by the chat app and by the headless router. */
@Composable
fun ServerPanel(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val state by runtime.serverState.collectAsState()
    var portText by remember { mutableStateOf(runtime.settings.port.toString()) }
    var allowNetwork by remember { mutableStateOf(runtime.settings.allowNetwork) }
    var apiKey by remember { mutableStateOf(runtime.settings.apiKey) }

    fun startServer() {
        runtime.settings.port = portText.toIntOrNull() ?: RuntimeSettings.DEFAULT_PORT
        runtime.settings.allowNetwork = allowNetwork
        ServerService.start(context)
    }

    val notificationPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { startServer() }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text(
            if (state.running) "Server running" else "Server stopped",
            style = MaterialTheme.typography.titleMedium,
        )
        state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        state.baseUrl?.let { Text("Base URL: $it") }
        Text("API key: $apiKey", style = MaterialTheme.typography.bodySmall)

        OutlinedTextField(
            value = portText,
            onValueChange = { portText = it.filter(Char::isDigit).take(5) },
            label = { Text("Port") },
            singleLine = true,
            enabled = !state.running,
            modifier = Modifier.fillMaxWidth(),
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            Switch(
                checked = allowNetwork,
                onCheckedChange = { allowNetwork = it },
                enabled = !state.running,
            )
            Text("  Allow other devices on this network")
        }
        Text(
            if (allowNetwork) {
                "Other devices can reach the server, but only with the API key."
            } else {
                "Only apps on this device can reach the server."
            },
            style = MaterialTheme.typography.bodySmall,
        )

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (state.running) {
                Button(onClick = { ServerService.stop(context) }) { Text("Stop") }
            } else {
                Button(onClick = {
                    if (needsNotificationPermission(context)) {
                        notificationPermission.launch("android.permission.POST_NOTIFICATIONS")
                    } else {
                        startServer()
                    }
                }) { Text("Start") }
            }
            OutlinedButton(
                onClick = { apiKey = runtime.settings.rotateApiKey() },
                enabled = !state.running,
            ) { Text("New API key") }
        }
    }
}

private fun needsNotificationPermission(context: Context): Boolean =
    Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
        context.checkSelfPermission("android.permission.POST_NOTIFICATIONS") != PackageManager.PERMISSION_GRANTED
