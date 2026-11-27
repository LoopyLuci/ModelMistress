package com.modelmistress.core.voice

/** Where the live conversation is. The screen shows it in words. */
enum class LivePhase { STARTING, LISTENING, THINKING, SPEAKING, STOPPED }

/** What the phone's recogniser, its speech and the chat report to the live conversation. */
sealed interface LiveInput {
    /** The owner has started, and the microphone is allowed. */
    data object Start : LiveInput

    /** The recogniser's words so far, while the owner is still speaking. */
    data class Heard(val text: String) : LiveInput

    /** The recogniser's words for one utterance. Blank when nothing was heard. */
    data class Said(val text: String) : LiveInput

    /**
     * The recogniser failed. A transient failure is retried, up to a few times in a row. A permanent one ends the conversation
     * and shows [message].
     */
    data class Failed(val transient: Boolean, val message: String? = null) : LiveInput

    /** The chat was asked to answer. [started] is false when it did not start the reply. */
    data class Sent(val started: Boolean) : LiveInput

    /** The chat's reply as it stands. [busy] says whether it is still being written. */
    data class Reply(val text: String?, val busy: Boolean, val error: String?) : LiveInput

    /** One sentence has been said, or could not be. */
    data object Spoken : LiveInput

    /** The owner pressed Interrupt. */
    data object Interrupt : LiveInput

    /** The owner turned the microphone on or off. Off stops listening; a reply that is running keeps playing. */
    data class Mic(val on: Boolean) : LiveInput

    /** The hold on a held sentence has run out, so it is sent unless it was cancelled. */
    data object HoldEnded : LiveInput

    /** The owner pressed Cancel send. The held sentence is dropped. */
    data object CancelHold : LiveInput

    /** The owner turned edit before send on or off. On, a held sentence waits until the owner sends it. */
    data class EditBeforeSend(val on: Boolean) : LiveInput

    /** The owner changed the held sentence in the box. It is kept as it is typed, so spaces are not lost. */
    data class EditHeld(val text: String) : LiveInput

    /** The owner pressed Send now on the held sentence. */
    data object SendHeld : LiveInput

    /** The owner ended the conversation. */
    data object End : LiveInput
}

/** What the app does next for the live conversation. */
sealed interface LiveEffect {
    /** Start the recogniser after [delayMs] milliseconds. */
    data class Listen(val delayMs: Long) : LiveEffect

    /** Stop the recogniser, and any restart that is waiting to happen. */
    data object CancelListening : LiveEffect

    /**
     * Show [text] as a sentence about to be sent. When [timed], the app answers with [LiveInput.HoldEnded] once the hold has run
     * out. A new hold replaces the old one, so more speech extends the wait. When not [timed], the owner sends it by hand, so
     * nothing ends the hold on its own.
     */
    data class Hold(val text: String, val timed: Boolean = true) : LiveEffect

    /** Send the owner's words to the chat. The app answers with [LiveInput.Sent]. */
    data class Send(val text: String) : LiveEffect

    /** Stop the reply the chat is writing. */
    data object StopReply : LiveEffect

    /** Say one sentence. The app answers with [LiveInput.Spoken] once it has been said, or could not be. */
    data class Say(val sentence: String) : LiveEffect

    /** Stop speech and drop what is queued to be said. */
    data object StopSpeech : LiveEffect

    /**
     * The owner corrected the reply to [question]. [answer] is the reply as the owner heard it: all of it when it finished, or
     * the part heard before the owner talked over it. [correction] is what the owner said is right. The app keeps it for Misty.
     */
    data class Record(val question: String, val answer: String, val correction: String) : LiveEffect
}

/**
 * The rules of a live conversation, with no Android in them.
 *
 * The phone listens for the whole conversation, not only between replies, unless the owner has turned the microphone off. A
 * sentence the owner finishes is held for a moment, so the owner can cancel it, and then sent to the chat. The reply is said
 * sentence by sentence, and the owner can talk over it to stop it. What the phone says is taken out of what it hears, so its
 * own voice does not start a turn. Talk that arrives while the chat is still writing waits its turn, and then is held too.
 *
 * The recogniser cannot tell the owner from other voices in the room, so a held sentence can be taken back: the owner presses
 * Cancel send, or says "cancel", "never mind" or "forget it" as the next sentence.
 *
 * With edit before send on, a held sentence is not sent by the timer. The owner can change its text first, and sends it with
 * Send now.
 *
 * A sentence that starts with a correction phrase ("correction", "that's wrong", "not quite") is not sent to the chat. It is
 * recorded against the last reply the owner heard: the words after the phrase are the correction, or, when there are none, the
 * next sentence is. A correction said while a reply is still being written waits until that reply has finished.
 *
 * Feed it each [LiveInput] in order, and carry out the [LiveEffect]s it returns. Read [phase], [heard], [error], [micOn],
 * [awaitingCorrection], [held] and [notice] for the screen.
 */
class LiveTurns {
    private var begun = false
    private var active = false
    private var listening = false
    private var listenDelay = 0L
    private var failures = 0

    /** Whether the chat is writing a reply, as far as the app has reported it. */
    private var chatBusy = false

    /** A turn the owner started is not finished: it was sent, or it waits to be sent. */
    private var replying = false

    /** The chat has begun the turn. Until it has, a finished reply in the chat is the one from before. */
    private var turnStarted = false

    /** The chat is still finishing a reply that was stopped. Its text is not spoken. */
    private var stale = false

    /** What the owner said while a reply was running, to hold and send once it has finished. */
    private var waiting: String? = null
    private var cutter = SentenceCutter()

    /** Sentences handed to speech that have not been said yet. */
    private var toSay = 0

    /**
     * The most recent words the phone has said, in order, so its own voice is not taken as the owner's. They are kept after the
     * speech ends, because the recogniser's result for a sentence can arrive after the sentence has finished.
     */
    private val spoken = ArrayDeque<String>()

    /** The question of the turn the chat was last sent, and the reply so far for the turn in progress. */
    private var turnQuestion = ""
    private var turnReply = ""

    /** The turn the owner heard last, as (question, reply). A correction refers to it. Null when there is none. */
    private var lastTurn: Pair<String, String>? = null

    /** A correction said while a reply was still being written. It is recorded once that reply has finished. */
    private var queuedCorrection: String? = null

    /** The owner said only the correction phrase. The next sentence is the correction. */
    var awaitingCorrection = false
        private set

    /** Whether the microphone is on. When it is off the phone does not listen, but a reply that is running still plays. */
    var micOn = true
        private set

    /** A finished sentence that waits to be sent, so the owner can cancel it, or null. */
    var held: String? = null
        private set

    /** Whether the owner sends held sentences by hand. On, the hold timer does not send them. */
    var editBeforeSend = false
        private set

    /** What was saved last, for the screen, or null. */
    var notice: String? = null
        private set

    /** What the owner said, as it is heard, for the screen. */
    var heard: String = ""
        private set

    /** What went wrong, for the screen, or null. */
    var error: String? = null
        private set

    val phase: LivePhase
        get() = when {
            !begun -> LivePhase.STARTING
            !active -> LivePhase.STOPPED
            toSay > 0 -> LivePhase.SPEAKING
            replying -> LivePhase.THINKING
            else -> LivePhase.LISTENING
        }

    fun handle(input: LiveInput): List<LiveEffect> {
        val out = mutableListOf<LiveEffect>()
        when (input) {
            LiveInput.Start -> {
                begun = true
                active = true
                error = null
                spoken.clear()
            }
            is LiveInput.Heard -> if (active && micOn) {
                failures = 0
                val text = input.text.trim()
                // The phone's own voice is not shown as the owner's words.
                heard = if (echoLike(text)) "" else text
                // Talking over the phone while it is speaking stops the reply. While the chat is only writing, the words wait.
                if (toSay > 0 && interrupts(text)) interruptReply(out)
            }
            is LiveInput.Said -> onSaid(input.text.trim(), out)
            is LiveInput.Failed -> onFailed(input, out)
            is LiveInput.Sent -> onSent(input.started, out)
            is LiveInput.Reply -> onReply(input, out)
            LiveInput.Spoken -> if (toSay > 0) toSay--
            LiveInput.Interrupt -> if (active) {
                waiting = null
                held = null
                queuedCorrection = null
                awaitingCorrection = false
                interruptReply(out)
            }
            is LiveInput.Mic -> micChanged(input.on, out)
            LiveInput.HoldEnded -> holdEnded(out)
            LiveInput.CancelHold -> {
                held = null
            }
            is LiveInput.EditBeforeSend -> editBeforeSendChanged(input.on, out)
            is LiveInput.EditHeld -> if (held != null) held = input.text
            LiveInput.SendHeld -> sendHeld(out)
            LiveInput.End -> end(out)
        }
        if (active && micOn && !listening) {
            listening = true
            out += LiveEffect.Listen(listenDelay)
            listenDelay = 0L
        }
        return out
    }

    private fun onSaid(text: String, out: MutableList<LiveEffect>) {
        listening = false
        if (!active || !micOn) return
        failures = 0
        if (text.isEmpty()) {
            heard = ""
            listenDelay = RESTART_MS
            return
        }
        // The phone hearing its own voice is not the owner talking, whether or not it is still speaking.
        if (echoLike(text)) {
            heard = ""
            return
        }
        heard = text
        error = null
        onSentence(text, out)
    }

    /** A sentence the owner finished: a correction of the last reply, or a message for the chat. */
    private fun onSentence(text: String, out: MutableList<LiveEffect>) {
        // The owner takes back a sentence that is being held by saying so. Other sentences are not taken back.
        if (held != null && CANCEL_START.containsMatchIn(text)) {
            held = null
            return
        }
        val found = correctionIn(text)
        if (found == null && !awaitingCorrection) {
            sendOrWait(text, out)
            return
        }
        awaitingCorrection = false
        val correction = found ?: text
        if (correction.isEmpty()) {
            // Only the phrase was said. The next sentence is the correction.
            awaitingCorrection = true
            return
        }
        if (toSay > 0) interruptReply(out)
        if (replying) {
            queuedCorrection = join(queuedCorrection, correction)
        } else {
            correct(correction, out)
        }
    }

    private fun onFailed(input: LiveInput.Failed, out: MutableList<LiveEffect>) {
        begun = true
        if (!input.transient) {
            error = input.message ?: GAVE_UP
            end(out)
            return
        }
        listening = false
        // A failure while the microphone is off is the recogniser stopping, not a fault to retry.
        if (!active || !micOn) return
        failures++
        if (failures > MAX_FAILURES) {
            error = GAVE_UP
            end(out)
            return
        }
        listenDelay = RETRY_MS
    }

    private fun onSent(started: Boolean, out: MutableList<LiveEffect>) {
        if (started) {
            chatBusy = true
            turnStarted = true
            // The owner stopped the turn before the chat began it: stop the reply that has just started.
            if (!replying) stopChat(out)
        } else {
            replying = false
            turnStarted = false
            waiting = null
            error = NOT_SENT
            val queued = queuedCorrection
            queuedCorrection = null
            if (queued != null) correct(queued, out)
        }
    }

    private fun onReply(input: LiveInput.Reply, out: MutableList<LiveEffect>) {
        chatBusy = input.busy
        if (stale) {
            // A reply the owner stopped. Its text is ignored until the chat has finished it.
            if (!input.busy) {
                stale = false
                resume(out)
            }
            return
        }
        if (!replying) return
        if (input.busy) {
            turnStarted = true
            input.text?.let {
                turnReply = it
                say(cutter.cut(it), out)
            }
            return
        }
        // The chat has not begun this turn yet: what it holds is the reply from before.
        if (!turnStarted) return
        val finished = input.text?.takeIf { it.isNotEmpty() }
        finished?.let { say(cutter.cut(it, final = true), out) }
        input.error?.let { error = it }
        lastTurn = finished?.takeIf { it.isNotBlank() }?.let { turnQuestion to it }
        val queued = queuedCorrection
        queuedCorrection = null
        replying = false
        turnStarted = false
        turnReply = ""
        cutter = SentenceCutter()
        if (queued != null) correct(queued, out)
        resume(out)
    }

    private fun micChanged(on: Boolean, out: MutableList<LiveEffect>) {
        micOn = on
        if (!on) {
            heard = ""
            if (listening) {
                listening = false
                out += LiveEffect.CancelListening
            }
        }
    }

    /** Holds the owner's sentence before it is sent, or queues it while a reply is being written. Talking over a reply stops it. */
    private fun sendOrWait(text: String, out: MutableList<LiveEffect>) {
        if (toSay > 0) interruptReply(out)
        if (replying) {
            waiting = join(waiting, text)
            return
        }
        hold(text, out)
    }

    /** Holds [text] before it is sent. More speech joins the held sentence and extends the hold. */
    private fun hold(text: String, out: MutableList<LiveEffect>) {
        val now = join(held, text)
        held = now
        out += LiveEffect.Hold(now, timed = !editBeforeSend)
    }

    /** The hold has run out: the held sentence is sent, unless it was cancelled. In edit mode the owner sends it instead. */
    private fun holdEnded(out: MutableList<LiveEffect>) {
        if (editBeforeSend) return
        sendHeld(out)
    }

    /** Sends the held sentence as it now stands. A blank one is dropped. */
    private fun sendHeld(out: MutableList<LiveEffect>) {
        val text = held ?: return
        held = null
        if (text.isBlank()) return
        startTurn(text.trim(), out)
    }

    /** Turns edit before send on or off. Turned off, a sentence still held is sent when its timer runs out, as before. */
    private fun editBeforeSendChanged(on: Boolean, out: MutableList<LiveEffect>) {
        editBeforeSend = on
        val text = held ?: return
        if (on) return
        if (text.isBlank()) {
            held = null
        } else {
            out += LiveEffect.Hold(text, timed = true)
        }
    }

    private fun startTurn(text: String, out: MutableList<LiveEffect>) {
        replying = true
        turnStarted = false
        turnReply = ""
        cutter = SentenceCutter()
        notice = null
        if (chatBusy || stale) {
            waiting = join(waiting, text)
        } else {
            turnQuestion = text
            out += LiveEffect.Send(text)
        }
    }

    /** Holds what was said while the chat was busy, once the chat is free. */
    private fun resume(out: MutableList<LiveEffect>) {
        val next = waiting ?: return
        waiting = null
        hold(next, out)
    }

    /** Records [correction] against the last turn the owner heard. Only called when no reply is being written. */
    private fun correct(correction: String, out: MutableList<LiveEffect>) {
        val turn = lastTurn
        if (turn == null) {
            say(listOf(NO_REPLY), out)
            return
        }
        out += LiveEffect.Record(turn.first, turn.second, correction)
        notice = SAVED
        say(listOf(NOTED), out)
    }

    private fun say(sentences: List<String>, out: MutableList<LiveEffect>) {
        for (sentence in sentences) {
            toSay++
            remember(words(sentence))
            out += LiveEffect.Say(sentence)
        }
    }

    /** Keeps the most recent words the phone has said. */
    private fun remember(said: List<String>) {
        spoken.addAll(said)
        while (spoken.size > SPOKEN_WORDS) spoken.removeFirst()
    }

    /** Stops speech and the chat's reply. What the owner said after that is still heard and sent. */
    private fun interruptReply(out: MutableList<LiveEffect>) {
        if (toSay > 0) {
            toSay = 0
            out += LiveEffect.StopSpeech
        }
        // What the owner heard of the reply, before talking over it, is the reply a correction refers to.
        if (replying && turnReply.isNotBlank()) lastTurn = turnQuestion to turnReply
        stopChat(out)
        replying = false
        turnStarted = false
        turnReply = ""
        cutter = SentenceCutter()
    }

    private fun stopChat(out: MutableList<LiveEffect>) {
        if (chatBusy && !stale) {
            stale = true
            out += LiveEffect.StopReply
        }
    }

    private fun end(out: MutableList<LiveEffect>) {
        if (listening) {
            listening = false
            out += LiveEffect.CancelListening
        }
        if (toSay > 0) {
            toSay = 0
            out += LiveEffect.StopSpeech
        }
        stopChat(out)
        active = false
        replying = false
        turnStarted = false
        waiting = null
        held = null
        queuedCorrection = null
        awaitingCorrection = false
        spoken.clear()
        cutter = SentenceCutter()
    }

    /**
     * The correction a sentence starts with, or null when it does not start with one. The words after the phrase are the
     * correction; they are blank when the owner said only the phrase.
     */
    private fun correctionIn(text: String): String? {
        val match = CORRECTION_START.find(text) ?: return null
        return text.substring(match.range.last + 1).trim()
    }

    /**
     * Marks each word of [all] that is part of a pair of words the phone has just said, one after the other. A pair is the
     * smallest unit that counts, so one common word such as "you" does not mark anything by itself.
     */
    private fun echoed(all: List<String>): BooleanArray {
        val marks = BooleanArray(all.size)
        if (spoken.size < 2) return marks
        val said = spoken.zipWithNext { first, second -> "$first $second" }.toHashSet()
        for (i in 0 until all.size - 1) {
            if ("${all[i]} ${all[i + 1]}" in said) {
                marks[i] = true
                marks[i + 1] = true
            }
        }
        return marks
    }

    /**
     * True when the words are the phone's own voice: nearly all of them run on from what it has just said. A recognition error
     * breaks only the pairs around it, so a long echo with one wrong word still counts.
     */
    private fun echoLike(text: String): Boolean {
        val all = words(text)
        if (all.isEmpty()) return true
        val share = echoed(all).count { it }.toDouble() / all.size
        return share >= ECHO_SHARE || (all.size >= ECHO_LONG_WORDS && share >= ECHO_LONG_SHARE)
    }

    /** True when the owner is talking over the phone: at least two words that are not the phone's own voice. */
    private fun interrupts(text: String): Boolean {
        val all = words(text)
        val own = echoed(all)
        return all.indices.count { !own[it] } >= 2 && !echoLike(text)
    }

    private fun words(text: String): List<String> = text.lowercase().split(NOT_A_WORD).filter { it.isNotEmpty() }

    private fun join(first: String?, second: String): String = if (first.isNullOrEmpty()) second else "$first $second"

    private companion object {
        const val RESTART_MS = 300L
        const val RETRY_MS = 800L
        const val MAX_FAILURES = 4
        /** Share of words that must be the phone's own, for a short utterance to count as its echo. */
        const val ECHO_SHARE = 0.9

        /** For a longer utterance, a lower share is enough: a recognition error is then expected. */
        const val ECHO_LONG_WORDS = 5
        const val ECHO_LONG_SHARE = 0.7

        /** How many of the phone's recent words are kept to recognise its own voice. */
        const val SPOKEN_WORDS = 160
        const val GAVE_UP = "Speech recognition stopped working on this phone."
        const val NOT_SENT = "Your message was not sent. Choose a model in the Chat, then speak again."
        const val NO_REPLY = "There is no reply to correct yet."
        const val NOTED = "Noted."
        const val SAVED = "Saved. Misty uses it for similar questions."
        val NOT_A_WORD = Regex("[^\\p{L}\\p{N}]+")

        /**
         * A correction phrase at the start of a sentence, after any short lead-in such as "no" or "wait". Only the start counts,
         * so "it is not quite what I meant" is still a message. The apostrophe may be straight or curly.
         */
        val CORRECTION_START = Regex(
            """^(?:(?:no|wait|actually|sorry|hang on|hold on|um|uh|well|hmm)\b[\s,.:;!?-]*)*""" +
                """(?:correction|not quite|(?:that[’']?s|that is|you[’']?re|you are) (?:wrong|not right))\b[\s,.:;!?-]*""",
            RegexOption.IGNORE_CASE,
        )

        /** Words that take back a held sentence, at its start: "cancel", "never mind", "forget it", "ignore that". */
        val CANCEL_START = Regex("""^(?:cancel|never mind|forget (?:it|that)|ignore that)\b""", RegexOption.IGNORE_CASE)
    }
}
