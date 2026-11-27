package com.modelmistress.router

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.modelmistress.runtime.ModelRuntime
import com.modelmistress.runtime.ModelsPanel
import com.modelmistress.runtime.ServerPanel

/** The headless build's only screen: start the server, keep the models, and nothing else. */
class RouterActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val runtime = ModelRuntime.get(applicationContext)
        setContent {
            MaterialTheme {
                Column(
                    Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    Text("ModelMistress Router", style = MaterialTheme.typography.headlineSmall)
                    Text(
                        "Serves the models on this device to other apps through an OpenAI-compatible API.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    ServerPanel(runtime)
                    HorizontalDivider()
                    Text("Models", style = MaterialTheme.typography.titleMedium)
                    ModelsPanel(runtime)
                }
            }
        }
    }
}
