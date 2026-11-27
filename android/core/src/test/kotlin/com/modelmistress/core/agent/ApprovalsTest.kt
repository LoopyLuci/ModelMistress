package com.modelmistress.core.agent

import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ApprovalsTest {
    /** Answers the first request that arrives, as the owner would on the router's screen. */
    private fun ownerAnswers(approvals: Approvals, allow: Boolean): Thread = Thread {
        val deadline = System.currentTimeMillis() + 5_000
        while (approvals.pending().isEmpty() && System.currentTimeMillis() < deadline) Thread.sleep(5)
        approvals.pending().forEach { approvals.answer(it.id, allow) }
    }.apply { start() }

    @Test
    fun refusesWhenTheOwnerDoesNotAnswerInTime() {
        val approvals = Approvals(timeoutMillis = 100)
        assertFalse(approvals.ask("MobileAgents", "Delete the model?"))
        assertTrue(approvals.pending().isEmpty())
    }

    @Test
    fun allowsWhenTheOwnerAllowsInTime() {
        val approvals = Approvals(timeoutMillis = 5_000)
        val owner = ownerAnswers(approvals, allow = true)
        assertTrue(approvals.ask("MobileAgents", "Delete the model?"))
        owner.join()
    }

    @Test
    fun refusesWhenTheOwnerRefuses() {
        val approvals = Approvals(timeoutMillis = 5_000)
        val owner = ownerAnswers(approvals, allow = false)
        assertFalse(approvals.ask("MobileAgents", "Delete the model?"))
        owner.join()
    }

    @Test
    fun showsWhatIsWaitingAndTellsTheScreenEachChange() {
        val seen = mutableListOf<List<Approval>>()
        val approvals = Approvals(timeoutMillis = 5_000, onChange = { seen += it })
        val waiting = AtomicReference<List<Approval>>()
        val asker = Thread {
            approvals.ask("Mobile\u0007Agents", "Download a model", "from a repo")
        }.apply { start() }
        val deadline = System.currentTimeMillis() + 5_000
        while (approvals.pending().isEmpty() && System.currentTimeMillis() < deadline) Thread.sleep(5)
        waiting.set(approvals.pending())
        approvals.pending().forEach { approvals.answer(it.id, true) }
        asker.join()

        val shown = waiting.get().single()
        assertEquals("MobileAgents", shown.client)
        assertEquals("Download a model", shown.title)
        assertEquals("from a repo", shown.detail)
        // One change when the request arrives and one when it is answered and removed.
        assertEquals(listOf(1, 0), seen.map { it.size })
    }

    @Test
    fun answeringAnUnknownRequestDoesNothing() {
        val approvals = Approvals(timeoutMillis = 100)
        assertFalse(approvals.answer("no-such-request", allow = true))
    }

    @Test
    fun refuseAllUnblocksEveryWaitingRequest() {
        val approvals = Approvals(timeoutMillis = 60_000)
        val results = mutableListOf<Boolean>()
        val asker = Thread { results += approvals.ask("app", "Export training data?") }.apply { start() }
        val deadline = System.currentTimeMillis() + 5_000
        while (approvals.pending().isEmpty() && System.currentTimeMillis() < deadline) Thread.sleep(5)
        approvals.refuseAll()
        asker.join(5_000)
        assertEquals(listOf(false), results)
    }

    @Test
    fun cleansWhatAnAppSentBeforeItIsShown() {
        assertEquals("ab", Approvals.clean("a\u0007b   ", max = 5, fallback = "x"))
        assertEquals("An app", Approvals.clean("   ", max = 10, fallback = "An app"))
        assertEquals("abc", Approvals.clean("abcdef", max = 3, fallback = "x"))
    }
}
