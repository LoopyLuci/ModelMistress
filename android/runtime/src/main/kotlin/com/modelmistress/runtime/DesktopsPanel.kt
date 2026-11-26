package com.modelmistress.runtime

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.modelmistress.core.remote.DesktopClient
import com.modelmistress.core.remote.DesktopHub
import com.modelmistress.runtime.overlay.OverlayStatus
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The desktops this device is paired with. A desktop is added in one of two ways: through the Transferd overlay, with the
 * invite it prints and a peer key (works from any network, no address needed); or by its address (LAN or Tailscale). Its
 * models then appear in the chat and in the server's model list.
 */
@Composable
fun DesktopsPanel(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val scope = rememberCoroutineScope()
    var hubs by remember { mutableStateOf(runtime.desktops.list()) }
    var status by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    var overlayStatus by remember { mutableStateOf<OverlayStatus?>(null) }

    var overlayName by remember { mutableStateOf("") }
    var invite by remember { mutableStateOf("") }
    var overlayKey by remember { mutableStateOf("") }
    var overlayError by remember { mutableStateOf<String?>(null) }
    var overlayNotice by remember { mutableStateOf<String?>(null) }
    var pairing by remember { mutableStateOf(false) }

    var name by remember { mutableStateOf("") }
    var address by remember { mutableStateOf("") }
    var key by remember { mutableStateOf("") }
    var formError by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(Unit) {
        overlayStatus = withContext(Dispatchers.IO) { runtime.overlay.status() }
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text("Desktops", style = MaterialTheme.typography.titleMedium)
        Text(
            "Models that run on a computer you own. Through the Transferd overlay, this phone reaches the desktop from " +
                "any network; the desktop publishes its hub to paired devices only, and its hub stays on the desktop's " +
                "loopback. Or give the desktop's address (LAN or Tailscale).",
            style = MaterialTheme.typography.bodySmall,
        )
        overlayStatus?.let { s ->
            Text(
                when {
                    s.error != null -> "This phone's overlay: ${s.error}"
                    s.running -> "This phone's overlay is running" +
                        if (s.relays.isEmpty()) " with no relay." else " on ${s.relays.size} relay(s)."
                    else -> "This phone's overlay starts when a desktop is first reached."
                },
                style = MaterialTheme.typography.bodySmall,
            )
        }

        if (hubs.isEmpty()) {
            Text("No desktops yet.", style = MaterialTheme.typography.bodyMedium)
        }
        hubs.forEach { hub ->
            Column {
                Text(hub.name, style = MaterialTheme.typography.labelLarge)
                Text(
                    if (hub.throughOverlay) "Through the Transferd overlay (${hub.overlayId?.take(12)}…)" else hub.url,
                    style = MaterialTheme.typography.bodySmall,
                )
                status[hub.name]?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                Row {
                    TextButton(onClick = {
                        status = status + (hub.name to "Checking…")
                        scope.launch {
                            val text = withContext(Dispatchers.IO) {
                                try {
                                    val count = DesktopClient(hub, runtime.overlay).models().size
                                    "Reached. $count model" + (if (count == 1) "" else "s") + " available."
                                } catch (error: RuntimeException) {
                                    error.message ?: "The desktop did not answer."
                                }
                            }
                            status = status + (hub.name to text)
                        }
                    }) { Text("Check") }
                    TextButton(onClick = {
                        runtime.desktops.remove(hub.name)
                        hubs = runtime.desktops.list()
                        status = status - hub.name
                    }) { Text("Remove") }
                }
            }
        }

        Text("Add through the Transferd overlay", style = MaterialTheme.typography.labelLarge)
        Text(
            "On the desktop, run `mm-overlay serve` with `--invite-ttl 900` and copy the invite it prints. Make a peer " +
                "key for this phone with `model-mistress keys create <name>`, and enter it here.",
            style = MaterialTheme.typography.bodySmall,
        )
        OutlinedTextField(
            value = overlayName,
            onValueChange = { overlayName = it },
            label = { Text("Name, for example home") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = invite,
            onValueChange = { invite = it },
            label = { Text("Invite from the desktop") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = overlayKey,
            onValueChange = { overlayKey = it },
            label = { Text("Peer key") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth(),
        )
        overlayError?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        overlayNotice?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        Button(
            enabled = !pairing,
            onClick = {
                val trimmedName = overlayName.trim()
                val trimmedInvite = invite.trim()
                val trimmedKey = overlayKey.trim()
                when {
                    trimmedName.isEmpty() -> overlayError = "Give the desktop a name."
                    trimmedInvite.isEmpty() -> overlayError = "Paste the invite the desktop printed."
                    trimmedKey.isEmpty() -> overlayError = "Enter the peer key the desktop made for this phone."
                    else -> {
                        pairing = true
                        overlayError = null
                        overlayNotice = null
                        scope.launch {
                            val outcome = withContext(Dispatchers.IO) {
                                try {
                                    val paired = runtime.overlay.pair(trimmedInvite, trimmedName)
                                    runtime.desktops.save(DesktopHub.overlay(trimmedName, paired.nodeId, trimmedKey))
                                    Result.success(paired)
                                } catch (error: RuntimeException) {
                                    Result.failure(error)
                                } catch (error: java.io.IOException) {
                                    Result.failure(error)
                                }
                            }
                            pairing = false
                            outcome.onSuccess { paired ->
                                hubs = runtime.desktops.list()
                                overlayName = ""
                                invite = ""
                                overlayKey = ""
                                overlayNotice = if (paired.relaysToAdd.isEmpty()) {
                                    "Paired. The desktop is reached through the overlay."
                                } else {
                                    "Paired. Restart the app to register this phone on the desktop's relay: " +
                                        paired.relaysToAdd.joinToString(", ") + "."
                                }
                            }.onFailure { error ->
                                overlayError = error.message ?: "The desktop could not be paired."
                            }
                        }
                    }
                }
            },
        ) { Text(if (pairing) "Pairing…" else "Pair through the overlay") }

        Text("Or by address", style = MaterialTheme.typography.labelLarge)
        Text(
            "Over Tailscale, use the computer's Tailscale address and keep Tailscale connected on this phone.",
            style = MaterialTheme.typography.bodySmall,
        )
        OutlinedTextField(
            value = name,
            onValueChange = { name = it },
            label = { Text("Name, for example home") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = address,
            onValueChange = { address = it },
            label = { Text("Address, for example http://100.101.98.77:8000") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = key,
            onValueChange = { key = it },
            label = { Text("Peer key") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth(),
        )
        formError?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        Button(onClick = {
            try {
                runtime.desktops.save(DesktopHub.of(name, address, key))
                hubs = runtime.desktops.list()
                name = ""
                address = ""
                key = ""
                formError = null
            } catch (error: IllegalArgumentException) {
                formError = error.message
            }
        }) { Text("Save desktop") }
    }
}
