package com.modelmistress.app

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.modelmistress.core.chat.Role
import com.modelmistress.core.voice.LiveEffect
import com.modelmistress.core.voice.LiveInput
import com.modelmistress.core.voice.LivePhase
import com.modelmistress.core.voice.LiveTurns
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** What the live screen shows: the phase, what the phone heard, and what went wrong. */
data class LiveUi(
    val phase: LivePhase = LivePhase.STARTING,
    /** What the phone heard, as it is heard, until the owner's sentence is sent. */
    val heard: String = "",
    val error: String? = null,
    /** Whether the microphone is on. Off, the phone does not listen. */
    val micOn: Boolean = true,
    /** The owner said only the correction phrase. The next sentence is the correction. */
    val awaitingCorrection: Boolean = false,
    /** What was saved last, such as a correction, or null. */
    val notice: String? = null,
    /** A finished sentence that waits to be sent, so the owner can cancel it, or null. */
    val held: String? = null,
    /** Whether the owner sends held sentences by hand, from an editable box. */
    val editBeforeSend: Boolean = false,
)

/**
 * The spoken conversation. The phone listens for the whole conversation, sends each sentence the owner finishes to the chat,
 * and says the reply aloud sentence by sentence. The owner can talk over the reply to stop it. The turn rules are in
 * [LiveTurns]; this class only feeds them what the recogniser, the speech and the chat report, and carries out what they ask.
 *
 * Everything here runs on the main thread: the recogniser and the speech engine call back on it, and their reports are queued
 * there too.
 */
class LiveConversation(
    private val context: Context,
    /** Sends the heard sentence with a note for the model. Returns false when the chat did not start the reply. */
    private val send: (text: String, note: String?) -> Boolean,
    private val stopReply: () -> Unit,
    /** What the camera shows now, for the model, or null when the camera is off. */
    private val cameraNote: () -> String?,
    /** Saves the owner's correction of a reply: the question, the reply as the owner heard it, the correction, and the camera then. */
    private val onCorrection: (question: String, answer: String, correction: String, camera: String?) -> Unit,
) {
    private val main = Handler(Looper.getMainLooper())
    private val turns = LiveTurns()
    private val inbox = ArrayDeque<LiveInput>()
    private var draining = false
    private var live = false
    private var recognizer: SpeechRecognizer? = null

    /** Set after a recogniser error that needs a fresh one. It is made before the next listen. */
    private var renew = false

    /** What the camera showed when the question now being answered was sent. A correction is saved with it. */
    private var askedCamera: String? = null

    /** Starts the recogniser after a delay. It is one object so that a cancel can remove it. */
    private val restart = Runnable { startRecognizer() }

    /** Ends the hold on a held sentence, so the turn rules send it. A new hold replaces this one. */
    private val commit = Runnable { post(LiveInput.HoldEnded) }
    private val speaker = Speaker(context, onSpoken = { post(LiveInput.Spoken) })
    private val output = MutableStateFlow(LiveUi())
    val state: StateFlow<LiveUi> = output.asStateFlow()

    private val listenIntent: Intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
        .putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
        .putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
        .putExtra(RecognizerIntent.EXTRA_MAX_RESULTS, 1)
        // A pause of this length ends a sentence. Shorter pauses are part of the owner's sentence.
        .putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_COMPLETE_SILENCE_LENGTH_MILLIS, COMPLETE_SILENCE_MS)
        // Sounds shorter than this are not speech, so a cough or a click does not become a message.
        .putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_MINIMUM_LENGTH_MILLIS, MINIMUM_SPEECH_MS)

    /** A recogniser that has heard nothing for a long time is cancelled, and the phone listens again. */
    private val watchdog = Runnable {
        if (live) {
            recognizer?.cancel()
            post(LiveInput.Said(""))
        }
    }

    private val listener = object : RecognitionListener {
        override fun onReadyForSpeech(params: Bundle?) {}
        override fun onBeginningOfSpeech() = touch()
        override fun onRmsChanged(rmsdB: Float) {}
        override fun onBufferReceived(buffer: ByteArray?) {}
        override fun onEndOfSpeech() {}
        override fun onEvent(eventType: Int, params: Bundle?) {}

        override fun onPartialResults(partialResults: Bundle?) {
            if (!live) return
            touch()
            partialResults.firstText()?.let { post(LiveInput.Heard(it)) }
        }

        override fun onResults(results: Bundle?) {
            if (!live) return
            main.removeCallbacks(watchdog)
            post(LiveInput.Said(results.firstText().orEmpty()))
        }

        override fun onError(error: Int) {
            if (!live) return
            main.removeCallbacks(watchdog)
            when (error) {
                // Nothing was said: listen again straight away.
                SpeechRecognizer.ERROR_NO_MATCH, SpeechRecognizer.ERROR_SPEECH_TIMEOUT -> post(LiveInput.Said(""))
                SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS ->
                    post(LiveInput.Failed(transient = false, message = MIC_DENIED))
                SpeechRecognizer.ERROR_RECOGNIZER_BUSY, SpeechRecognizer.ERROR_CLIENT -> {
                    renew = true
                    post(LiveInput.Failed(transient = true))
                }
                else -> post(LiveInput.Failed(transient = true))
            }
        }
    }

    /** Starts the conversation. Call it once the microphone is allowed. */
    fun start() {
        if (live) return
        live = true
        if (SpeechRecognizer.isRecognitionAvailable(context)) {
            recognizer = newRecognizer()
            post(LiveInput.Start)
        } else {
            post(LiveInput.Start)
            post(LiveInput.Failed(transient = false, message = NO_RECOGNIZER))
        }
    }

    /** Shows [message] and stops the conversation. */
    fun fail(message: String) {
        post(LiveInput.Failed(transient = false, message = message))
        teardown()
    }

    /** Ends the conversation: the microphone, the speech and any reply in progress all stop. */
    fun end() {
        post(LiveInput.End)
        teardown()
    }

    /** Stops the reply in progress and what is being said. The phone keeps listening. */
    fun interrupt() {
        post(LiveInput.Interrupt)
    }

    /** Turns the microphone on or off. Off, the phone stops listening; a reply that is still playing goes on. */
    fun setMic(on: Boolean) {
        post(LiveInput.Mic(on))
    }

    /** Takes back the sentence that is waiting to be sent. The owner can also say "cancel". */
    fun cancelHold() {
        post(LiveInput.CancelHold)
    }

    /** Turns edit before send on or off. On, a held sentence waits in a box until the owner sends it. */
    fun setEditBeforeSend(on: Boolean) {
        post(LiveInput.EditBeforeSend(on))
    }

    /** Changes the held sentence to [text], as the owner types it. */
    fun editHeld(text: String) {
        post(LiveInput.EditHeld(text))
    }

    /** Sends the held sentence now, as it stands in the box. */
    fun sendHeld() {
        post(LiveInput.SendHeld)
    }

    /**
     * Takes in the chat's reply as it is written. [text] is the reply so far, [busy] says whether it is still being written, and
     * [error] is what went wrong, if anything.
     */
    fun onReply(text: String?, busy: Boolean, error: String?) {
        post(LiveInput.Reply(text, busy, error))
    }

    /** Hands [input] to the turn rules, one at a time. Inputs that arrive while one is being carried out wait their turn. */
    private fun post(input: LiveInput) {
        inbox.addLast(input)
        if (draining) return
        draining = true
        try {
            while (inbox.isNotEmpty()) {
                val effects = turns.handle(inbox.removeFirst())
                output.value = LiveUi(
                    phase = turns.phase,
                    heard = turns.heard,
                    error = turns.error,
                    micOn = turns.micOn,
                    awaitingCorrection = turns.awaitingCorrection,
                    notice = turns.notice,
                    held = turns.held,
                    editBeforeSend = turns.editBeforeSend,
                )
                effects.forEach(::execute)
            }
        } finally {
            draining = false
        }
    }

    private fun execute(effect: LiveEffect) {
        when (effect) {
            is LiveEffect.Listen ->
                if (effect.delayMs <= 0L) startRecognizer() else main.postDelayed(restart, effect.delayMs)
            LiveEffect.CancelListening -> {
                // A restart that is waiting must not start the recogniser after the owner has turned the microphone off.
                main.removeCallbacks(restart)
                main.removeCallbacks(watchdog)
                recognizer?.cancel()
            }
            is LiveEffect.Hold -> {
                main.removeCallbacks(commit)
                // In edit mode the owner sends the held sentence by hand, so nothing is timed.
                if (effect.timed) main.postDelayed(commit, HOLD_MS)
            }
            is LiveEffect.Send -> {
                val camera = cameraNote()
                askedCamera = camera
                post(LiveInput.Sent(send(effect.text, note(camera))))
            }
            LiveEffect.StopReply -> stopReply()
            is LiveEffect.Say -> if (!speaker.enqueue(effect.sentence)) post(LiveInput.Spoken)
            LiveEffect.StopSpeech -> speaker.stop()
            is LiveEffect.Record -> onCorrection(effect.question, effect.answer, effect.correction, askedCamera)
        }
    }

    private fun startRecognizer() {
        if (!live) return
        if (renew) {
            renew = false
            recognizer?.destroy()
            recognizer = newRecognizer()
        }
        val rec = recognizer ?: return
        touch()
        try {
            rec.startListening(listenIntent)
        } catch (error: RuntimeException) {
            post(LiveInput.Failed(transient = true))
        }
    }

    private fun newRecognizer(): SpeechRecognizer =
        SpeechRecognizer.createSpeechRecognizer(context).also { it.setRecognitionListener(listener) }

    private fun touch() {
        main.removeCallbacks(watchdog)
        main.postDelayed(watchdog, WATCHDOG_MS)
    }

    private fun note(camera: String?): String = listOfNotNull(SPOKEN_STYLE, camera).joinToString("\n\n")

    private fun teardown() {
        live = false
        main.removeCallbacks(watchdog)
        main.removeCallbacks(restart)
        main.removeCallbacks(commit)
        recognizer?.destroy()
        recognizer = null
        speaker.shutdown()
    }

    private companion object {
        const val WATCHDOG_MS = 45_000L

        /** How long a finished sentence waits, so the owner can take it back, before it is sent. */
        const val HOLD_MS = 1_500L
        const val COMPLETE_SILENCE_MS = 1_200L
        const val MINIMUM_SPEECH_MS = 400L
        const val MIC_DENIED = "The microphone is not allowed for ModelMistress. Allow it in the phone's settings."
        const val NO_RECOGNIZER = "Speech recognition is not available on this phone. Type your message instead."

        /** Asks the model for short spoken answers. It is sent with the message and is not kept in the chat. */
        const val SPOKEN_STYLE = "This is a spoken conversation. Answer in one to three short sentences, in plain words, " +
            "with no lists, headings or markdown."

        fun Bundle?.firstText(): String? = this?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()
    }
}

/**
 * The camera during a live conversation. It shows a preview, and reads a frame about every second and a half: labels and any
 * text in the frame, on this phone. The model gets that description with each message. It does not see the picture itself.
 */
class LiveCamera(private val context: Context) {
    private val analysis: ExecutorService = Executors.newSingleThreadExecutor()
    private val description = MutableStateFlow<String?>(null)
    private val running = MutableStateFlow(false)
    private var provider: ProcessCameraProvider? = null
    private var lastRead = 0L

    /** The latest description of the camera, or null before the first frame is read. */
    val latest: StateFlow<String?> = description.asStateFlow()

    /** Whether the camera is on. */
    val isRunning: StateFlow<Boolean> = running.asStateFlow()

    /** Starts the camera into [preview], bound to [owner]. [onError] is told when it cannot start. */
    fun start(owner: LifecycleOwner, preview: PreviewView, onError: (String) -> Unit) {
        val future = ProcessCameraProvider.getInstance(context)
        future.addListener({
            try {
                val cameraProvider = future.get()
                provider = cameraProvider
                val view = Preview.Builder().build().also { it.surfaceProvider = preview.surfaceProvider }
                val frames = ImageAnalysis.Builder()
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_RGBA_8888)
                    .build()
                frames.setAnalyzer(analysis) { frame -> read(frame) }
                cameraProvider.unbindAll()
                cameraProvider.bindToLifecycle(owner, CameraSelector.DEFAULT_BACK_CAMERA, view, frames)
                running.value = true
            } catch (error: Exception) {
                running.value = false
                onError("The camera could not start on this phone (${error.message ?: "unknown error"}).")
            }
        }, ContextCompat.getMainExecutor(context))
    }

    /** Turns the camera off and forgets its last description. */
    fun stop() {
        runCatching { provider?.unbindAll() }
        running.value = false
        description.value = null
    }

    /** The note the model reads about the camera now, or null when the camera is off. */
    fun note(): String? {
        if (!running.value) return null
        val seen = description.value ?: return "The camera is on, but no frame has been read yet."
        return "What the camera shows now, from on-device recognition of its latest frame (labels and any text, not a full " +
            "description):\n$seen"
    }

    /** Runs on the analysis thread. A frame that cannot be read is skipped. */
    private fun read(frame: ImageProxy) {
        try {
            val now = SystemClock.elapsedRealtime()
            if (now - lastRead >= FRAME_INTERVAL_MS) {
                lastRead = now
                val bitmap = frame.toBitmap()
                try {
                    description.value = Attachments.describeBitmap(bitmap, frame.imageInfo.rotationDegrees)
                } finally {
                    bitmap.recycle()
                }
            }
        } catch (error: Exception) {
            // The frame is skipped; the next one is read.
        } finally {
            frame.close()
        }
    }

    fun shutdown() {
        stop()
        analysis.shutdown()
    }

    private companion object {
        const val FRAME_INTERVAL_MS = 1_500L
    }
}

/**
 * The live conversation screen. It starts listening when the microphone is allowed, and the camera can be turned on for the
 * conversation. Everything said and answered is kept in the chat, as typed messages are.
 */
@Composable
fun LiveScreen(viewModel: ChatViewModel, onEnd: () -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val owner = LocalLifecycleOwner.current
    val chat by viewModel.uiState.collectAsState()
    val camera = remember { LiveCamera(context.applicationContext) }
    val conversation = remember {
        LiveConversation(
            context = context.applicationContext,
            send = { text, note ->
                viewModel.send(typed = text, note = note)
                viewModel.uiState.value.busy
            },
            stopReply = viewModel::stop,
            cameraNote = camera::note,
            onCorrection = viewModel::recordCorrection,
        )
    }
    val ui by conversation.state.collectAsState()
    val cameraText by camera.latest.collectAsState()
    val cameraOn by camera.isRunning.collectAsState()
    val preview = remember { PreviewView(context) }
    var cameraNotice by remember { mutableStateOf<String?>(null) }

    val permissions = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { granted ->
        val audio = granted[Manifest.permission.RECORD_AUDIO] == true || hasPermission(context, Manifest.permission.RECORD_AUDIO)
        if (audio) {
            conversation.start()
        } else {
            conversation.fail("Live conversation needs the microphone. Allow it for ModelMistress in the phone's settings.")
        }
    }
    val cameraPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            cameraNotice = null
            camera.start(owner, preview) { problem -> cameraNotice = problem }
        } else {
            cameraNotice = "The camera is not allowed for ModelMistress. Allow it in the phone's settings."
        }
    }

    // Asks for the microphone, and the camera for the camera toggle. Already-allowed permissions pass straight through.
    LaunchedEffect(Unit) {
        permissions.launch(arrayOf(Manifest.permission.RECORD_AUDIO))
    }
    // The reply's text is spoken as it is written. Once the chat has finished, the rest is spoken and listening goes on.
    LaunchedEffect(chat.busy, chat.streaming, chat.messages, chat.error) {
        val reply = if (chat.busy) chat.streaming else chat.messages.lastOrNull()?.takeIf { it.role == Role.ASSISTANT }?.content
        conversation.onReply(reply, busy = chat.busy, error = chat.error)
    }
    DisposableEffect(Unit) {
        onDispose {
            conversation.end()
            camera.shutdown()
        }
    }

    fun toggleCamera() {
        if (cameraOn) {
            camera.stop()
            return
        }
        if (hasPermission(context, Manifest.permission.CAMERA)) {
            camera.start(owner, preview) { problem -> cameraNotice = problem }
        } else {
            cameraPermission.launch(Manifest.permission.CAMERA)
        }
    }

    Column(
        modifier = modifier.fillMaxSize().padding(12.dp).verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text("Live conversation", style = MaterialTheme.typography.titleMedium)
        Text(phaseLine(ui.phase, ui.micOn), style = MaterialTheme.typography.bodyLarge)
        if (ui.awaitingCorrection) {
            Text(
                "Say the correction now. It is saved against the reply, and not sent to the model.",
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        ui.notice?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        ui.held?.let { held ->
            if (ui.editBeforeSend) {
                OutlinedTextField(
                    value = held,
                    onValueChange = conversation::editHeld,
                    label = { Text("Edit, then send") },
                    modifier = Modifier.fillMaxWidth(),
                )
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = conversation::sendHeld, enabled = held.isNotBlank()) { Text("Send now") }
                    OutlinedButton(onClick = conversation::cancelHold) { Text("Cancel send") }
                }
            } else {
                Text("Sending soon: $held", style = MaterialTheme.typography.bodyMedium)
                OutlinedButton(onClick = conversation::cancelHold) { Text("Cancel send") }
            }
        }
        if (cameraOn) {
            AndroidView(factory = { preview }, modifier = Modifier.fillMaxWidth().height(220.dp))
            Text(
                cameraText?.let { "The camera reads: ${it.lineSequence().first()}" } ?: "The camera is on. Reading the first frame…",
                style = MaterialTheme.typography.labelSmall,
            )
        }
        cameraNotice?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        if (ui.heard.isNotBlank()) {
            Text("You: ${ui.heard}", style = MaterialTheme.typography.bodyMedium)
        }
        val reply = if (chat.busy) chat.streaming else chat.messages.lastOrNull { it.role == Role.ASSISTANT }?.content
        if (!reply.isNullOrBlank()) {
            Text("Model: $reply", style = MaterialTheme.typography.bodyMedium)
        }
        ui.error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
        // Listen is filled while the microphone is on and outlined while it is paused. Pausing does not end the reply.
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(onClick = ::toggleCamera) { Text(if (cameraOn) "Camera off" else "Camera on") }
            if (ui.micOn) {
                Button(onClick = { conversation.setMic(false) }) { Text("Listen") }
            } else {
                OutlinedButton(onClick = { conversation.setMic(true) }) { Text("Listen") }
            }
            OutlinedButton(onClick = conversation::interrupt) { Text("Interrupt") }
            // Edit before send is filled while on: a finished sentence then waits in a box until it is sent by hand.
            if (ui.editBeforeSend) {
                Button(onClick = { conversation.setEditBeforeSend(false) }) { Text("Edit before send") }
            } else {
                OutlinedButton(onClick = { conversation.setEditBeforeSend(true) }) { Text("Edit before send") }
            }
            Button(onClick = onEnd) { Text("End") }
        }
        Text(
            "The phone keeps listening while it speaks, so you can talk over it to stop it. It tries to ignore its own voice. " +
                "Say \"correction\", \"that's wrong\" or \"not quite\" to correct the last reply. The correction is saved for Misty " +
                "and is not sent as a message. Listen pauses the microphone, and a reply still playing goes on. " +
                "A sentence waits about a second and a half before it is sent, so you can take it back with Cancel send or by " +
                "saying \"cancel\". With Edit before send on, the sentence waits in a box for Send now, and you can change it first. " +
                "Speech is heard and read aloud by the phone's own speech engines. The camera gives the model labels and any " +
                "text in the frame, not a full description of the picture.",
            style = MaterialTheme.typography.labelSmall,
        )
    }
}

/** The phase in words. With the microphone off, the phone says so: a reply that is still playing carries on. */
private fun phaseLine(phase: LivePhase, micOn: Boolean): String = when (phase) {
    LivePhase.STARTING -> "Starting…"
    LivePhase.LISTENING ->
        if (micOn) "Listening. Just talk; I answer when you pause." else "Mic is off. Press Listen to start listening again."
    LivePhase.THINKING ->
        if (micOn) "Thinking… Keep talking and your next sentence waits its turn." else "Thinking… Mic is off."
    LivePhase.SPEAKING ->
        if (micOn) "Speaking… Talk over me to stop." else "Speaking… Mic is off, so I cannot hear you."
    LivePhase.STOPPED -> "Stopped."
}

private fun hasPermission(context: Context, permission: String): Boolean =
    ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED
