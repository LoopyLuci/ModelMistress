package com.modelmistress.app

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.Role
import com.modelmistress.runtime.ModelRuntime
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class ChatUiState(
    val messages: List<ChatMessage> = emptyList(),
    val streaming: String? = null,
    val draft: String = "",
    val models: List<String> = emptyList(),
    val selectedId: String? = null,
    val busy: Boolean = false,
    val error: String? = null,
)

/** The chat on this device. The history is kept in memory for as long as the app runs. */
class ChatViewModel(private val runtime: ModelRuntime) : ViewModel() {
    private val state = MutableStateFlow(ChatUiState())
    val uiState: StateFlow<ChatUiState> = state.asStateFlow()
    private var reply: Job? = null

    init {
        refreshModels()
    }

    fun refreshModels() {
        viewModelScope.launch {
            val ids = withContext(Dispatchers.IO) { runtime.library.list().map { it.id } }
            state.update { current ->
                val selected = listOfNotNull(current.selectedId, runtime.settings.defaultModelId, ids.firstOrNull())
                    .firstOrNull { it in ids }
                current.copy(models = ids, selectedId = selected)
            }
        }
    }

    fun selectModel(id: String) {
        state.update { it.copy(selectedId = id) }
    }

    fun setDraft(text: String) {
        state.update { it.copy(draft = text) }
    }

    fun send() {
        val current = state.value
        val modelId = current.selectedId ?: return
        val text = current.draft.trim()
        if (text.isEmpty() || current.busy) return
        val history = current.messages + ChatMessage(Role.USER, text)
        state.update { it.copy(messages = history, draft = "", busy = true, error = null, streaming = "") }
        reply = viewModelScope.launch {
            val partial = StringBuilder()
            var failure: String? = null
            try {
                withContext(Dispatchers.IO) {
                    val backend = runtime.hub.backend(modelId)
                        ?: throw IllegalStateException("That model is no longer installed.")
                    backend.complete(history, GenerationOptions(maxTokens = 512, temperature = 0.7f)) { piece ->
                        partial.append(piece)
                        state.update { it.copy(streaming = partial.toString()) }
                        isActive
                    }
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                failure = error.message ?: "The reply failed."
            } finally {
                val kept = if (partial.isEmpty()) {
                    history
                } else {
                    history + ChatMessage(Role.ASSISTANT, partial.toString())
                }
                state.update { it.copy(messages = kept, streaming = null, busy = false, error = failure) }
            }
        }
    }

    fun stop() {
        reply?.cancel()
    }

    companion object {
        fun factory(runtime: ModelRuntime): ViewModelProvider.Factory = viewModelFactory {
            initializer { ChatViewModel(runtime) }
        }
    }
}
