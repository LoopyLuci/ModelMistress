package com.modelmistress.app

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
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val runtime = ModelRuntime.get(applicationContext)
        setContent {
            MaterialTheme {
                AppRoot(runtime)
            }
        }
    }
}

private enum class Tab(val title: String, val glyph: String) {
    CHAT("Chat", "C"),
    MODELS("Models", "M"),
    SERVER("Server", "S"),
}

@Composable
private fun AppRoot(runtime: ModelRuntime) {
    val context = LocalContext.current
    var tab by remember { mutableStateOf(Tab.CHAT) }
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
