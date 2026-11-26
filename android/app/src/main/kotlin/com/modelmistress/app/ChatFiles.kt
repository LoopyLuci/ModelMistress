package com.modelmistress.app

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import com.modelmistress.core.chat.AttachmentKind
import com.modelmistress.core.chat.ChatAttachment
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Opens a file of the chat's sandbox in another app on the phone, the one that shows its type. Returns a message to show when
 * no app can, or null when the file opened.
 */
fun openSandboxFile(context: Context, file: File, name: String): String? {
    return try {
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
        val intent = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, Attachments.mimeFor(name))
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        context.startActivity(intent)
        null
    } catch (error: ActivityNotFoundException) {
        "No app on this phone opens $name."
    } catch (error: IllegalArgumentException) {
        "$name cannot be shared with other apps."
    }
}

/** The files a message carries, under its text: a picture with its own preview, and a document as a button. Each one opens. */
@Composable
fun AttachmentRow(
    attachments: List<ChatAttachment>,
    fileFor: (ChatAttachment) -> File?,
    onOpen: (File, String) -> Unit,
    onMissing: () -> Unit,
) {
    if (attachments.isEmpty()) return
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        attachments.forEach { attachment ->
            val file = fileFor(attachment)
            if (attachment.kind == AttachmentKind.PICTURE) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    PictureThumbnail(file)
                    Column(modifier = Modifier.weight(1f)) {
                        Text(attachment.label, style = MaterialTheme.typography.labelLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text(attachment.file, style = MaterialTheme.typography.labelSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                    OutlinedButton(onClick = { if (file != null) onOpen(file, attachment.file) else onMissing() }) { Text("Open") }
                }
            } else {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        "📄 ${attachment.label}",
                        style = MaterialTheme.typography.bodyMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    OutlinedButton(onClick = { if (file != null) onOpen(file, attachment.file) else onMissing() }) { Text("Open") }
                }
            }
        }
    }
}

/** A small preview of a picture in the sandbox. Shows nothing while it loads, or when the file is gone. */
@Composable
fun PictureThumbnail(file: File?) {
    val image by produceState<ImageBitmap?>(initialValue = null, file) {
        value = file?.let { withContext(Dispatchers.IO) { Attachments.decodeScaled(it, maxSide = THUMBNAIL_SIDE)?.asImageBitmap() } }
    }
    image?.let { loaded ->
        Image(bitmap = loaded, contentDescription = null, modifier = Modifier.size(72.dp))
    }
}

private const val THUMBNAIL_SIDE = 320

/**
 * The files of the chat's sandbox: the sandbox's name, its files (each opens), the sandboxes it is linked to, and the ones it
 * could link to. Files come in from the device here, or from the paperclip in the message box.
 */
@Composable
fun SandboxPanel(state: ChatUiState, viewModel: ChatViewModel) {
    val context = LocalContext.current
    val view = state.sandbox
    val pick = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        viewModel.addToSandbox(context.applicationContext, uris)
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Sandbox", style = MaterialTheme.typography.titleSmall)
        Text(
            "The files this chat's tools read or change stay in its sandbox, a folder on this phone that other apps cannot " +
                "reach. A chat sees another sandbox only when the two are linked.",
            style = MaterialTheme.typography.labelSmall,
        )
        Text(view?.name ?: "Loading…", style = MaterialTheme.typography.bodyMedium)
        Button(onClick = { pick.launch(arrayOf("*/*")) }) { Text("Add files from this device") }
        state.sandboxStatus?.let { Text(it, style = MaterialTheme.typography.labelSmall) }
        if (view == null) return@Column
        if (view.files.isEmpty()) {
            Text("No files in this sandbox yet.", style = MaterialTheme.typography.bodySmall)
        }
        view.files.forEach { file ->
            Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(file.name, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(formatBytes(file.bytes), style = MaterialTheme.typography.labelSmall)
                }
                OutlinedButton(onClick = {
                    val target = viewModel.sandboxFile(file.name)
                    val problem = if (target == null) "${file.name} is no longer in the sandbox." else openSandboxFile(context, target, file.name)
                    if (problem != null) viewModel.reportError(problem)
                }) { Text("Open") }
                TextButton(onClick = { viewModel.removeSandboxFile(file.name) }) { Text("Remove") }
            }
        }
        if (view.linked.isNotEmpty()) {
            Text("Linked sandboxes", style = MaterialTheme.typography.labelLarge)
            view.linked.forEach { link ->
                Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        linkLabel(link),
                        style = MaterialTheme.typography.bodyMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(onClick = { viewModel.unlinkSandbox(link.id) }) { Text("Unlink") }
                }
            }
        }
        if (view.linkable.isNotEmpty()) {
            Text("Link another sandbox", style = MaterialTheme.typography.labelLarge)
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                view.linkable.forEach { link ->
                    OutlinedButton(onClick = { viewModel.linkSandbox(link.id) }) { Text("Link ${linkLabel(link)}") }
                }
            }
        }
    }
}

private fun linkLabel(link: SandboxLink): String = link.chat?.let { "${link.name} (${it})" } ?: link.name

/** A size for the files list: bytes, kilobytes or megabytes. */
fun formatBytes(bytes: Long): String = when {
    bytes >= 1_000_000 -> "%.1f MB".format(bytes / 1_000_000.0)
    bytes >= 1_000 -> "%.0f KB".format(bytes / 1_000.0)
    else -> "$bytes B"
}
