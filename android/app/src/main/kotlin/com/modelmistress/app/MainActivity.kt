package com.modelmistress.app

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.modelmistress.runtime.ModelRuntime
import com.modelmistress.runtime.ModelsPanel
import com.modelmistress.runtime.ServerPanel

class MainActivity : ComponentActivity() {
    private var openRequest by mutableStateOf<OpenRequest?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val runtime = ModelRuntime.get(applicationContext)
        openRequest = requestFrom(intent)
        setContent {
            MaterialTheme {
                AppRoot(runtime, openRequest)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        openRequest = requestFrom(intent)
    }

    private fun requestFrom(intent: Intent?): OpenRequest? =
        intent?.getStringExtra(OPEN_TAB_EXTRA)?.let { OpenRequest(it) }

    companion object {
        /** Another app, such as MobileAgents, sends this extra to open a tab. "models" opens the Models tab. */
        const val OPEN_TAB_EXTRA = "com.modelmistress.app.extra.OPEN_TAB"
    }
}

/** One request to open a tab. Each request is a new object, so a repeated request still opens the tab. */
private class OpenRequest(val tab: String)

private enum class Tab(val title: String, val glyph: String) {
    CHAT("Chat", "C"),
    MODELS("Models", "M"),
    SERVER("Server", "S"),
}

@Composable
private fun AppRoot(runtime: ModelRuntime, request: OpenRequest?) {
    val context = LocalContext.current
    var tab by remember { mutableStateOf(Tab.CHAT) }
    LaunchedEffect(request) {
        if (request?.tab == "models") tab = Tab.MODELS
    }
    Scaffold(
        bottomBar = {
            NavigationBar {
                Tab.entries.forEach { item ->
                    NavigationBarItem(
                        selected = tab == item,
                        onClick = { tab = item },
                        icon = { Text(item.glyph) },
                        label = { Text(item.title) },
                    )
                }
            }
        },
    ) { padding ->
        when (tab) {
            Tab.CHAT -> ChatScreen(runtime, Modifier.padding(padding))
            Tab.MODELS -> Column(
                Modifier.padding(padding).padding(16.dp).verticalScroll(rememberScrollState()),
            ) {
                ModelsPanel(runtime, onExportTraining = { file -> shareTrainingFile(context, file) })
            }
            Tab.SERVER -> Column(
                Modifier.padding(padding).padding(16.dp).verticalScroll(rememberScrollState()),
            ) {
                ServerPanel(runtime)
            }
        }
    }
}
