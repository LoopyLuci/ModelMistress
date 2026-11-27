package com.modelmistress.core.agent

import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/** A request waiting for the owner's answer on this device. [client] names the app that asked, as that app sent it. */
data class Approval(
    val id: String,
    val client: String,
    val title: String,
    val detail: String,
    val askedAt: Long,
)

/**
 * The actions another app may not take on this device without the owner's OK: deleting or downloading a model, changing a
 * sandbox's files, exporting training data, and any file change a tool asks for.
 *
 * [ask] blocks the calling thread until the owner answers through [answer], or until the timeout passes, which refuses. [onChange]
 * is told the waiting requests each time that list changes, so the router's screen can show them.
 */
class Approvals(
    private val timeoutMillis: Long = DEFAULT_TIMEOUT_MILLIS,
    private val now: () -> Long = System::currentTimeMillis,
    private val onChange: (List<Approval>) -> Unit = {},
) {
    private class Waiting(val approval: Approval, val answer: CompletableFuture<Boolean>)

    private val lock = Any()
    private val waiting = LinkedHashMap<String, Waiting>()

    init {
        require(timeoutMillis > 0) { "The approval timeout must be positive." }
    }

    /** The requests waiting now, oldest first. */
    fun pending(): List<Approval> = synchronized(lock) { waiting.values.map { it.approval } }

    /** Asks the owner. True only when the owner allows it in time. Call it off the main thread. */
    fun ask(client: String, title: String, detail: String = ""): Boolean {
        val approval = Approval(
            id = UUID.randomUUID().toString(),
            client = clean(client, MAX_CLIENT, fallback = "An app"),
            title = clean(title, MAX_TITLE, fallback = "An app asks to do something"),
            detail = clean(detail, MAX_DETAIL, fallback = ""),
            askedAt = now(),
        )
        val answer = CompletableFuture<Boolean>()
        val snapshot = synchronized(lock) {
            waiting[approval.id] = Waiting(approval, answer)
            waiting.values.map { it.approval }
        }
        onChange(snapshot)
        return try {
            answer.get(timeoutMillis, TimeUnit.MILLISECONDS)
        } catch (timeout: TimeoutException) {
            false
        } catch (interrupted: InterruptedException) {
            Thread.currentThread().interrupt()
            false
        } finally {
            val after = synchronized(lock) {
                waiting.remove(approval.id)
                waiting.values.map { it.approval }
            }
            onChange(after)
        }
    }

    /** Answers the request [id]. False when no such request is waiting any more: it was answered or timed out. */
    fun answer(id: String, allow: Boolean): Boolean {
        val entry = synchronized(lock) { waiting[id] } ?: return false
        return entry.answer.complete(allow)
    }

    /** Refuses every request still waiting. The server calls this when it stops, so no thread is left waiting. */
    fun refuseAll() {
        val entries = synchronized(lock) { waiting.values.toList() }
        entries.forEach { it.answer.complete(false) }
    }

    companion object {
        /** How long a request waits for the owner before it is refused. */
        const val DEFAULT_TIMEOUT_MILLIS = 120_000L

        const val MAX_CLIENT = 40
        const val MAX_TITLE = 120
        const val MAX_DETAIL = 400

        /** Text an app sent, made safe to show: no control characters, and cut to [max] characters. */
        fun clean(text: String, max: Int, fallback: String): String =
            text.filter { !it.isISOControl() }.trim().take(max).ifEmpty { fallback }
    }
}
