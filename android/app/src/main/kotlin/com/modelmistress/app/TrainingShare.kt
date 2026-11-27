package com.modelmistress.app

import android.content.ClipData
import android.content.Context
import android.content.Intent
import androidx.core.content.FileProvider
import java.io.File

/**
 * Opens the phone's share sheet with [file], an exported training file in the app's cache. It goes through the app's
 * FileProvider, as the chat's photos do, so the app it is shared with can read this file and nothing else.
 */
fun shareTrainingFile(context: Context, file: File) {
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
    val send = Intent(Intent.ACTION_SEND)
        .setType("application/x-ndjson")
        .putExtra(Intent.EXTRA_STREAM, uri)
        .putExtra(Intent.EXTRA_SUBJECT, "ModelMistress training examples")
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    send.clipData = ClipData.newRawUri("ModelMistress training examples", uri)
    context.startActivity(Intent.createChooser(send, "Share training examples"))
}
