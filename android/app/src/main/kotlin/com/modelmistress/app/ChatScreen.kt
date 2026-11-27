package com.modelmistress.app

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.provider.MediaStore
import android.speech.RecognizerIntent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import androidx.lifecycle.viewmodel.compose.viewModel
import com.modelmistress.core.chat.ChatAttachment
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.ChatSummary
import com.modelmistress.core.chat.Role
import com.modelmistress.core.misty.FeedbackLabel
import com.modelmistress.core.misty.ToolFamily
import com.modelmistress.runtime.ModelRuntime
import java.io.File
import java.text.DateFormat
import java.util.Date
import java.util.UUID
import kotlinx.coroutines.launch

/** Documents and pictures the paperclip offers. The system's file picker shows them as a normal file explorer. */
private val ATTACHABLE_TYPES = arrayOf("text/*", "application/pdf", "image/*", "application/json")

/** The chat, or the live conversation when the owner starts one. Each chat works in its own sandbox. */
@Composable
fun ChatScreen(runtime: ModelRuntime, modifier: Modifier = Modifier) {
    val viewModel: ChatViewModel = viewModel(factory = ChatViewModel.factory(runtime))
    val state by viewModel.uiState.collectAsState()
    var live by remember { mutableStateOf(false) }
    if (live) {
        LiveScreen(viewModel = viewModel, onEnd = { live = false }, modifier = modifier)
    } else {
        ChatContent(viewModel = viewModel, state = state, onLive = { live = true }, modifier = modifier)
    }
}

@Composable
private fun ChatContent(
    viewModel: ChatViewModel,
    state: ChatUiState,
    onLive: () -> Unit,
    modifier: Modifier,
) {
    val context = LocalContext.current
    // The Chat tab is composed each time it opens, so the list picks up models installed or deleted on the Models tab.
    LaunchedEffect(Unit) { viewModel.refreshModels() }
    val transcript = state.messages + listOfNotNull(state.streaming?.let { ChatMessage(Role.ASSISTANT, it) })
    val speaker = rememberSpeaker()
    val scope = rememberCoroutineScope()

    // The list is laid out from the bottom: index 0 is the newest message, so a reply that grows keeps the bottom in view.
    val listState = rememberLazyListState()
    val atLatest by remember { derivedStateOf { listState.firstVisibleItemIndex == 0 && listState.firstVisibleItemScrollOffset <= 2 } }
    var following by remember { mutableStateOf(true) }
    // Scrolling up stops following. Coming back to the bottom starts it again, and the newest words stay in view.
    LaunchedEffect(atLatest, listState.isScrollInProgress) {
        if (atLatest) following = true else if (listState.isScrollInProgress) following = false
    }
    LaunchedEffect(transcript.size, state.streaming?.length) {
        if (following) listState.scrollToItem(0)
    }

    // Saved, not remembered: the camera can recreate this screen (the phone rotates while it is open), and the photo must still be found.
    var pendingPhoto by rememberSaveable { mutableStateOf<String?>(null) }
    val pickFiles = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris: List<Uri> ->
        viewModel.attachDocuments(context.applicationContext, uris)
    }
    val takePhoto = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { saved ->
        val file = pendingPhoto?.let { File(it) }
        pendingPhoto = null
        if (saved && file != null && file.length() > 0) viewModel.attachPhoto(file) else file?.delete()
    }
    val dictate = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val spoken = result.data?.getStringArrayListExtra(RecognizerIntent.EXTRA_RESULTS)?.firstOrNull()
        if (result.resultCode == Activity.RESULT_OK && !spoken.isNullOrBlank()) viewModel.appendToDraft(spoken)
    }

    val openFile: (File, String) -> Unit = { file, name ->
        openSandboxFile(context, file, name)?.let { viewModel.reportError(it) }
    }
    val fileOf: (ChatAttachment) -> File? = { attachment -> viewModel.sandboxFile(attachment.file) }
    val missing: () -> Unit = { viewModel.reportError("That file is no longer in the sandbox.") }

    Column(
        modifier = modifier.fillMaxSize().padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        FlowRow(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedButton(onClick = viewModel::newChat, enabled = !state.busy) { Text("New chat") }
            OutlinedButton(onClick = viewModel::toggleHistory) {
                Text(if (state.historyOpen) "Hide history" else "History")
            }
            OutlinedButton(onClick = viewModel::toggleFiles) {
                Text(if (state.filesOpen) "Hide files" else "Files")
            }
            Button(onClick = onLive, enabled = !state.busy && state.selectedId != null) { Text("Live") }
        }
        if (state.historyOpen) {
            HistoryList(
                history = state.history,
                busy = state.busy,
                onOpen = viewModel::openChat,
                onDelete = viewModel::deleteChat,
            )
        }
        if (state.filesOpen) {
            Column(
                modifier = Modifier.fillMaxWidth().heightIn(max = 340.dp).verticalScroll(rememberScrollState()),
            ) {
                SandboxPanel(state = state, viewModel = viewModel)
            }
        }
        ModelChooser(models = state.models, selected = state.selectedId, onSelect = viewModel::selectModel)
        if (state.models.isEmpty()) {
            Text("Import a model in the Models tab to start chatting.", style = MaterialTheme.typography.bodyMedium)
        }
        Column(modifier = Modifier.weight(1f).fillMaxWidth()) {
            LazyColumn(
                state = listState,
                reverseLayout = true,
                modifier = Modifier.weight(1f).fillMaxWidth(),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                items(transcript.size) { index ->
                    val message = transcript[transcript.lastIndex - index]
                    val streaming = state.busy && index == 0 && message.role == Role.ASSISTANT
                    Bubble(
                        message = message,
                        canRead = message.role == Role.ASSISTANT && !streaming,
                        onRead = { speaker.speak(message.content) },
                        fileOf = fileOf,
                        onOpen = openFile,
                        onMissing = missing,
                    )
                }
            }
            if (!atLatest && transcript.isNotEmpty()) {
                OutlinedButton(onClick = {
                    following = true
                    scope.launch { listState.animateScrollToItem(0) }
                }) { Text("Jump to latest") }
            }
        }
        state.toolNotes.forEach { note ->
            Text(note, style = MaterialTheme.typography.labelSmall)
        }
        state.rateable?.let { reply ->
            key(reply) {
                ReplyRating(onRate = { label, tools -> viewModel.rateReply(label, tools) })
            }
        }
        state.feedbackStatus?.let { Text(it, style = MaterialTheme.typography.labelSmall) }
        if (state.steers.isNotEmpty()) {
            state.steers.forEach { note ->
                Text("Note sent during the reply, taken in at its next step: $note", style = MaterialTheme.typography.labelSmall)
            }
        }
        if (state.queue.isNotEmpty()) {
            QueueList(queue = state.queue, onRemove = viewModel::removeQueued, onClear = viewModel::clearQueue)
        }
        if (state.attaching) {
            Text("Keeping and reading the attachment…", style = MaterialTheme.typography.labelSmall)
        }
        if (state.attachments.isNotEmpty()) {
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                state.attachments.forEachIndexed { index, attachment ->
                    OutlinedButton(onClick = { viewModel.removeAttachment(index) }) {
                        Text("${attachment.label} ×", maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                }
            }
        }
        state.error?.let {
            Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedButton(
                onClick = { pickFiles.launch(ATTACHABLE_TYPES) },
                enabled = !state.attaching,
                modifier = Modifier.semantics { contentDescription = "Attach a document or picture" },
            ) { Text("📎") }
            OutlinedButton(
                onClick = {
                    val intent = speechInputIntent()
                    if (intent.resolveActivity(context.packageManager) == null) {
                        viewModel.reportError("Speech input is not available on this phone.")
                    } else {
                        dictate.launch(intent)
                    }
                },
                modifier = Modifier.semantics { contentDescription = "Speak your message" },
            ) { Text("🎤") }
            OutlinedButton(
                onClick = {
                    if (Intent(MediaStore.ACTION_IMAGE_CAPTURE).resolveActivity(context.packageManager) == null) {
                        viewModel.reportError("No camera app is available on this phone.")
                    } else {
                        val folder = File(context.cacheDir, "photos").apply { mkdirs() }
                        val file = File(folder, "${UUID.randomUUID()}.jpg")
                        pendingPhoto = file.absolutePath
                        val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
                        try {
                            takePhoto.launch(uri)
                        } catch (error: ActivityNotFoundException) {
                            pendingPhoto = null
                            viewModel.reportError("No camera app is available on this phone.")
                        }
                    }
                },
                enabled = !state.attaching,
                modifier = Modifier.semantics { contentDescription = "Take a photo to recognise" },
            ) { Text("📷") }
        }
        OutlinedTextField(
            value = state.draft,
            onValueChange = viewModel::setDraft,
            placeholder = { Text(if (state.busy) "Add a note, or queue the next message" else "Message") },
            modifier = Modifier.fillMaxWidth(),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (state.busy) {
                OutlinedButton(
                    onClick = viewModel::steerDraft,
                    enabled = state.draft.isNotBlank(),
                ) { Text("Steer") }
                OutlinedButton(
                    onClick = viewModel::queueDraft,
                    enabled = state.draft.isNotBlank() || state.attachments.isNotEmpty(),
                ) { Text("Queue") }
                OutlinedButton(onClick = viewModel::stop) { Text("Stop") }
            } else {
                Button(
                    onClick = {
                        following = true
                        viewModel.send()
                    },
                    enabled = (state.draft.isNotBlank() || state.attachments.isNotEmpty()) &&
                        state.selectedId != null && !state.attaching,
                ) { Text("Send") }
            }
        }
    }

    state.pendingCall?.let { description ->
        AlertDialog(
            onDismissRequest = { viewModel.answerPendingCall(false) },
            title = { Text("Allow Misty?") },
            text = { Text(description) },
            confirmButton = { Button(onClick = { viewModel.answerPendingCall(true) }) { Text("Allow") } },
            dismissButton = { OutlinedButton(onClick = { viewModel.answerPendingCall(false) }) { Text("Refuse") } },
        )
    }
}

/**
 * The messages queued to send after the reply that is running. Each goes in order; the owner can remove one or clear them all.
 */
@Composable
private fun QueueList(queue: List<QueuedMessage>, onRemove: (Int) -> Unit, onClear: () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Queued: ${queue.size}", style = MaterialTheme.typography.labelLarge, modifier = Modifier.weight(1f))
            TextButton(onClick = onClear) { Text("Clear") }
        }
        queue.forEachIndexed { index, message ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                val attached = if (message.attachments.isEmpty()) "" else " (+${message.attachments.size} file)"
                Text(
                    "${index + 1}. ${message.text.ifEmpty { "Attachments" }}$attached",
                    style = MaterialTheme.typography.bodySmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = { onRemove(index) }) { Text("×") }
            }
        }
    }
}

/**
 * The owner's verdict on the last reply. "Should use a tool" opens the tool families, so the owner names the ones the
 * question needed, and Misty offers them for similar questions from now on.
 */
@Composable
private fun ReplyRating(onRate: (FeedbackLabel, List<String>) -> Unit) {
    var choosing by remember { mutableStateOf(false) }
    var chosen by remember { mutableStateOf(emptySet<ToolFamily>()) }
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("How was that reply?", style = MaterialTheme.typography.labelSmall)
        Row(
            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            OutlinedButton(onClick = { onRate(FeedbackLabel.GOOD, emptyList()) }) { Text("Good") }
            OutlinedButton(onClick = { onRate(FeedbackLabel.BAD, emptyList()) }) { Text("Bad") }
            OutlinedButton(onClick = { onRate(FeedbackLabel.NO_TOOLS, emptyList()) }) { Text("No tool needed") }
            OutlinedButton(onClick = { choosing = !choosing }) { Text("Should use a tool") }
        }
        if (choosing) {
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                ToolFamily.values().forEach { family ->
                    val on = family in chosen
                    val toggle = {
                        chosen = if (on) chosen - family else chosen + family
                    }
                    if (on) {
                        Button(onClick = toggle) { Text(family.label) }
                    } else {
                        OutlinedButton(onClick = toggle) { Text(family.label) }
                    }
                }
                Button(
                    enabled = chosen.isNotEmpty(),
                    onClick = { onRate(FeedbackLabel.NEEDS_TOOLS, chosen.flatMap { it.tools }.distinct()) },
                ) { Text("Save") }
            }
        }
    }
}

@Composable
private fun HistoryList(
    history: List<ChatSummary>,
    busy: Boolean,
    onOpen: (String) -> Unit,
    onDelete: (String) -> Unit,
) {
    if (history.isEmpty()) {
        Text("No saved chats yet. A chat is saved after its first reply.", style = MaterialTheme.typography.bodyMedium)
        return
    }
    LazyColumn(
        modifier = Modifier.fillMaxWidth().heightIn(max = 260.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        items(history, key = { it.id }) { chat ->
            Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        chat.title,
                        style = MaterialTheme.typography.bodyMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(historyLine(chat), style = MaterialTheme.typography.labelSmall)
                }
                OutlinedButton(onClick = { onOpen(chat.id) }, enabled = !busy) { Text("Open") }
                OutlinedButton(onClick = { onDelete(chat.id) }, enabled = !busy) { Text("Delete") }
            }
        }
    }
}

private fun historyLine(chat: ChatSummary): String {
    val date = DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(chat.updatedAt))
    val messages = if (chat.messageCount == 1) "1 message" else "${chat.messageCount} messages"
    return listOfNotNull(chat.modelId ?: "no model", messages, date).joinToString(" · ")
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
private fun Bubble(
    message: ChatMessage,
    canRead: Boolean,
    onRead: () -> Unit,
    fileOf: (ChatAttachment) -> File?,
    onOpen: (File, String) -> Unit,
    onMissing: () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            if (message.role == Role.USER) "You" else "Model",
            style = MaterialTheme.typography.labelSmall,
        )
        Text(message.content, style = MaterialTheme.typography.bodyMedium)
        AttachmentRow(attachments = message.attachments, fileFor = fileOf, onOpen = onOpen, onMissing = onMissing)
        if (canRead && message.content.isNotBlank()) {
            TextButton(onClick = onRead) { Text("Read aloud") }
        }
    }
}
