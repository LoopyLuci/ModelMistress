package com.modelmistress.runtime

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/**
 * The requests from other apps that wait for the owner: an action that changes this device, such as a download, a model
 * removal, or a file change. Each one is refused when the owner does not allow it in time.
 */
@Composable
fun ApprovalsPanel(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val waiting by runtime.pendingApprovals.collectAsState()
    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text("Waiting for you", style = MaterialTheme.typography.titleMedium)
        if (waiting.isEmpty()) {
            Text("No app is asking for anything.", style = MaterialTheme.typography.bodySmall)
        }
        waiting.forEach { approval ->
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text("${approval.client} asks", style = MaterialTheme.typography.labelLarge)
                Text(approval.title, style = MaterialTheme.typography.bodyMedium)
                if (approval.detail.isNotEmpty()) {
                    Text(approval.detail, style = MaterialTheme.typography.bodySmall)
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { runtime.approvals.answer(approval.id, allow = true) }) { Text("Allow") }
                    OutlinedButton(onClick = { runtime.approvals.answer(approval.id, allow = false) }) { Text("Deny") }
                }
            }
        }
    }
}
