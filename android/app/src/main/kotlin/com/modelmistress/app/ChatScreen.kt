package com.modelmistress.app

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.Role
import com.modelmistress.runtime.ModelRuntime

@Composable
fun ChatScreen(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val viewModel: ChatViewModel = viewModel(factory = ChatViewModel.factory(runtime))
    val state by viewModel.uiState.collectAsState()
    val transcript = state.messages + listOfNotNull(state.streaming?.let { ChatMessage(Role.ASSISTANT, it) })

    Column(
        modifier = modifier.fillMaxSize().padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        ModelChooser(models = state.models, selected = state.selectedId, onSelect = viewModel::selectModel)
        if (state.models.isEmpty()) {
            Text("Import a model in the Models tab to start chatting.", style = MaterialTheme.typography.bodyMedium)
        }
        LazyColumn(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            items(transcript) { message -> Bubble(message) }
        }
        state.error?.let {
            Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
        }
        Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = state.draft,
                onValueChange = viewModel::setDraft,
                placeholder = { Text("Message") },
                enabled = !state.busy,
                modifier = Modifier.weight(1f),
            )
            if (state.busy) {
                OutlinedButton(onClick = viewModel::stop) { Text("Stop") }
            } else {
                Button(
                    onClick = viewModel::send,
                    enabled = state.draft.isNotBlank() && state.selectedId != null,
                ) { Text("Send") }
            }
        }
    }
}

@Composable
private fun ModelChooser(models: List<String>, selected: String?, onSelect: (String) -> Unit) {
    if (models.isEmpty()) return
    Row(
        modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        models.forEach { id ->
            if (id == selected) {
                Button(onClick = { onSelect(id) }) { Text(id) }
            } else {
                OutlinedButton(onClick = { onSelect(id) }) { Text(id) }
            }
        }
    }
}

@Composable
private fun Bubble(message: ChatMessage) {
    Column {
        Text(
            if (message.role == Role.USER) "You" else "Model",
            style = MaterialTheme.typography.labelSmall,
        )
        Text(message.content, style = MaterialTheme.typography.bodyMedium)
    }
}
