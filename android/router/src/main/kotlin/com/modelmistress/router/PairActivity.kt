package com.modelmistress.router

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.modelmistress.runtime.ModelRuntime

/**
 * The pairing screen another app opens with [ACTION_PAIR], through startActivityForResult. It shows which app asks and waits for
 * the owner. On Allow it returns the router's origin ([EXTRA_BASE_URL], for example `http://127.0.0.1:8080`) and the API key
 * ([EXTRA_API_KEY]). On Deny, or when the owner backs out, it returns RESULT_CANCELED with nothing else.
 *
 * The OpenAI routes are at `<origin>/v1` and the router's own API at `<origin>/mm/v1`. The key is the owner's; it stays valid
 * until the owner makes a new one on the router's screen.
 */
class PairActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val runtime = ModelRuntime.get(applicationContext)
        val asker = appName(applicationContext, callingPackage)
        setContent {
            MaterialTheme {
                val state by runtime.serverState.collectAsState()
                val origin = state.baseUrl?.removeSuffix("/v1")
                Column(
                    Modifier.fillMaxSize().padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    Text("Pair with ModelMistress Router", style = MaterialTheme.typography.headlineSmall)
                    Text(
                        "$asker asks to use the models on this device through the router. It gets the router's address and " +
                            "its API key. It can then chat with the models here, and every action that changes the device " +
                            "still waits for your OK on the router's screen.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    if (origin == null) {
                        Text(
                            "The router's server is not running. Start it on the router's screen, then pair again.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.error,
                        )
                    } else {
                        Text("Address: $origin", style = MaterialTheme.typography.bodySmall)
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Button(
                            enabled = origin != null,
                            onClick = {
                                val reply = Intent()
                                    .putExtra(EXTRA_BASE_URL, origin)
                                    .putExtra(EXTRA_API_KEY, runtime.settings.apiKey)
                                setResult(RESULT_OK, reply)
                                finish()
                            },
                        ) { Text("Allow") }
                        OutlinedButton(onClick = {
                            setResult(RESULT_CANCELED)
                            finish()
                        }) { Text("Deny") }
                    }
                }
            }
        }
    }

    companion object {
        /** The action another app starts to pair. Its result carries [EXTRA_BASE_URL] and [EXTRA_API_KEY] on Allow. */
        const val ACTION_PAIR = "com.modelmistress.router.action.PAIR"

        /** The router's origin, without a path: the OpenAI routes sit under `/v1`, the router's API under `/mm/v1`. */
        const val EXTRA_BASE_URL = "com.modelmistress.router.extra.BASE_URL"

        /** The bearer key every request except health sends. */
        const val EXTRA_API_KEY = "com.modelmistress.router.extra.API_KEY"

        private fun appName(context: Context, packageName: String?): String {
            if (packageName == null) return "An app"
            return try {
                val info = context.packageManager.getApplicationInfo(packageName, 0)
                context.packageManager.getApplicationLabel(info).toString()
            } catch (missing: PackageManager.NameNotFoundException) {
                packageName
            }
        }
    }
}
