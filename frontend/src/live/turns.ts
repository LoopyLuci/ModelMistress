// The rules of a live conversation, with no browser in them. A port of the Android LiveTurns.
//
// The microphone listens for the whole conversation, not only between replies, unless the owner has turned it off. A
// sentence the owner finishes is held for a moment, so the owner can cancel it, and then sent to the chat. The reply is
// said sentence by sentence, and the owner can talk over it to stop it. What the phone says is taken out of what it hears,
// so its own voice does not start a turn. Talk that arrives while the chat is still writing waits its turn, and then is
// held too.
//
// The recogniser cannot tell the owner from other voices in the room, so a held sentence can be taken back: the owner
// presses Cancel send, or says "cancel", "never mind" or "forget it" as the next sentence.
//
// With edit before send on, a held sentence is not sent by the timer. The owner can change its text first, and sends it
// with Send now.
//
// A sentence that starts with a correction phrase ("correction", "that's wrong", "not quite") is not sent to the chat. It is
// recorded against the last reply the owner heard: the words after the phrase are the correction, or, when there are none,
// the next sentence is. A correction said while a reply is still being written waits until that reply has finished.
//
// Feed it each LiveInput in order, and carry out the LiveEffects it returns.

import { SentenceCutter } from './cutter'

export type LivePhase = 'starting' | 'listening' | 'thinking' | 'speaking' | 'stopped'

/** What the microphone, the speech, the chat and the owner report to the live conversation. */
export type LiveInput =
  | { kind: 'start' }
  /** The recogniser's words so far, while the owner is still speaking. */
  | { kind: 'heard'; text: string }
  /** The recogniser's words for one utterance. Blank when nothing was heard. */
  | { kind: 'said'; text: string }
  /** The recogniser failed. A transient failure is retried a few times in a row; a permanent one ends the conversation. */
  | { kind: 'failed'; transient: boolean; message?: string }
  /** The chat was asked to answer. `started` is false when it did not start the reply. */
  | { kind: 'sent'; started: boolean }
  /** The chat's reply as it stands. `busy` says whether it is still being written. */
  | { kind: 'reply'; text: string | null; busy: boolean; error: string | null }
  /** One sentence has been said, or could not be. */
  | { kind: 'spoken' }
  /** The owner pressed Interrupt. */
  | { kind: 'interrupt' }
  /** The owner turned the microphone on or off. Off stops listening; a reply that is running keeps playing. */
  | { kind: 'mic'; on: boolean }
  /** The hold on a held sentence has run out, so it is sent unless it was cancelled. */
  | { kind: 'holdEnded' }
  /** The owner pressed Cancel send. */
  | { kind: 'cancelHold' }
  /** The owner turned edit before send on or off. */
  | { kind: 'editBeforeSend'; on: boolean }
  /** The owner changed the held sentence in the box. */
  | { kind: 'editHeld'; text: string }
  /** The owner pressed Send now on the held sentence. */
  | { kind: 'sendHeld' }
  /** The owner ended the conversation. */
  | { kind: 'end' }

/** What the app does next for the live conversation. */
export type LiveEffect =
  /** Start listening again after `delayMs` milliseconds. */
  | { kind: 'listen'; delayMs: number }
  /** Stop listening, and any restart that is waiting to happen. */
  | { kind: 'cancelListening' }
  /**
   * Show `text` as a sentence about to be sent. When `timed`, the app answers with `holdEnded` once the hold has run out. A
   * new hold replaces the old one, so more speech extends the wait.
   */
  | { kind: 'hold'; text: string; timed: boolean }
  /** Send the owner's words to the chat. The app answers with `sent`. */
  | { kind: 'send'; text: string }
  /** Stop the reply the chat is writing. */
  | { kind: 'stopReply' }
  /** Say one sentence. The app answers with `spoken` once it has been said, or could not be. */
  | { kind: 'say'; sentence: string }
  /** Stop speech and drop what is queued to be said. */
  | { kind: 'stopSpeech' }
  /** The owner corrected the reply to `question`. `answer` is the reply as the owner heard it. */
  | { kind: 'record'; question: string; answer: string; correction: string }

const RESTART_MS = 300
const RETRY_MS = 800
const MAX_FAILURES = 4
/** Share of words that must be the phone's own, for a short utterance to count as its echo. */
const ECHO_SHARE = 0.9
/** For a longer utterance, a lower share is enough: a recognition error is then expected. */
const ECHO_LONG_WORDS = 5
const ECHO_LONG_SHARE = 0.7
/** How many of the phone's recent words are kept to recognise its own voice. */
const SPOKEN_WORDS = 160
export const GAVE_UP = 'Speech recognition stopped working on this phone.'
export const NOT_SENT = 'Your message was not sent. Choose a model in the Chat, then speak again.'
export const NO_REPLY = 'There is no reply to correct yet.'
export const NOTED = 'Noted.'
export const SAVED = 'Saved. Misty uses it for similar questions.'
/** How long a finished sentence waits, so the owner can take it back. */
export const HOLD_MS = 1500

const NOT_A_WORD = /[^\p{L}\p{N}]+/u

/**
 * A correction phrase at the start of a sentence, after any short lead-in such as "no" or "wait". Only the start counts, so
 * "it is not quite what I meant" is still a message. The apostrophe may be straight or curly.
 */
const CORRECTION_START = new RegExp(
  '^(?:(?:no|wait|actually|sorry|hang on|hold on|um|uh|well|hmm)\\b[\\s,.:;!?-]*)*' +
    "(?:correction|not quite|(?:that[’']?s|that is|you[’']?re|you are) (?:wrong|not right))\\b[\\s,.:;!?-]*",
  'i',
)

/** Words that take back a held sentence, at its start: "cancel", "never mind", "forget it", "ignore that". */
const CANCEL_START = /^(?:cancel|never mind|forget (?:it|that)|ignore that)\b/i

export function words(text: string): string[] {
  return text.toLowerCase().split(NOT_A_WORD).filter((w) => w.length > 0)
}

function join(first: string | null, second: string): string {
  return !first ? second : `${first} ${second}`
}

export class LiveTurns {
  private begun = false
  private active = false
  private listening = false
  private listenDelay = 0
  private failures = 0

  /** Whether the chat is writing a reply, as far as the app has reported it. */
  private chatBusy = false
  /** A turn the owner started is not finished: it was sent, or it waits to be sent. */
  private replying = false
  /** The chat has begun the turn. Until it has, a finished reply in the chat is the one from before. */
  private turnStarted = false
  /** The chat is still finishing a reply that was stopped. Its text is not spoken. */
  private stale = false
  /** What the owner said while a reply was running, to hold and send once it has finished. */
  private waiting: string | null = null
  private cutter = new SentenceCutter()
  /** Sentences handed to speech that have not been said yet. */
  private toSay = 0
  /** The most recent words the phone has said, in order, so its own voice is not taken as the owner's. */
  private spoken: string[] = []
  /** The question of the turn the chat was last sent, and the reply so far for the turn in progress. */
  private turnQuestion = ''
  private turnReply = ''
  /** The turn the owner heard last, as (question, reply). A correction refers to it. */
  private lastTurn: { question: string; reply: string } | null = null
  /** A correction said while a reply was still being written. It is recorded once that reply has finished. */
  private queuedCorrection: string | null = null

  /** The owner said only the correction phrase. The next sentence is the correction. */
  awaitingCorrection = false
  /** Whether the microphone is on. When it is off nothing is heard, but a reply that is running still plays. */
  micOn = true
  /** A finished sentence that waits to be sent, so the owner can cancel it, or null. */
  held: string | null = null
  /** Whether the owner sends held sentences by hand. On, the hold timer does not send them. */
  editBeforeSend = false
  /** What was saved last, for the screen, or null. */
  notice: string | null = null
  /** What the owner said, as it is heard, for the screen. */
  heard = ''
  /** What went wrong, for the screen, or null. */
  error: string | null = null

  get phase(): LivePhase {
    if (!this.begun) return 'starting'
    if (!this.active) return 'stopped'
    if (this.toSay > 0) return 'speaking'
    if (this.replying) return 'thinking'
    return 'listening'
  }

  handle(input: LiveInput): LiveEffect[] {
    const out: LiveEffect[] = []
    switch (input.kind) {
      case 'start':
        this.begun = true
        this.active = true
        this.error = null
        this.spoken = []
        break
      case 'heard':
        if (this.active && this.micOn) {
          this.failures = 0
          const text = input.text.trim()
          // The phone's own voice is not shown as the owner's words.
          this.heard = this.echoLike(text) ? '' : text
          // Talking over the phone while it is speaking stops the reply. While the chat is only writing, the words wait.
          if (this.toSay > 0 && this.interrupts(text)) this.interruptReply(out)
        }
        break
      case 'said':
        this.onSaid(input.text.trim(), out)
        break
      case 'failed':
        this.onFailed(input, out)
        break
      case 'sent':
        this.onSent(input.started, out)
        break
      case 'reply':
        this.onReply(input, out)
        break
      case 'spoken':
        if (this.toSay > 0) this.toSay--
        break
      case 'interrupt':
        if (this.active) {
          this.waiting = null
          this.held = null
          this.queuedCorrection = null
          this.awaitingCorrection = false
          this.interruptReply(out)
        }
        break
      case 'mic':
        this.micChanged(input.on, out)
        break
      case 'holdEnded':
        if (!this.editBeforeSend) this.sendHeld(out)
        break
      case 'cancelHold':
        this.held = null
        break
      case 'editBeforeSend':
        this.editBeforeSendChanged(input.on, out)
        break
      case 'editHeld':
        if (this.held !== null) this.held = input.text
        break
      case 'sendHeld':
        this.sendHeld(out)
        break
      case 'end':
        this.end(out)
        break
    }
    if (this.active && this.micOn && !this.listening) {
      this.listening = true
      out.push({ kind: 'listen', delayMs: this.listenDelay })
      this.listenDelay = 0
    }
    return out
  }

  private onSaid(text: string, out: LiveEffect[]) {
    this.listening = false
    if (!this.active || !this.micOn) return
    this.failures = 0
    if (text === '') {
      this.heard = ''
      this.listenDelay = RESTART_MS
      return
    }
    // The phone hearing its own voice is not the owner talking, whether or not it is still speaking.
    if (this.echoLike(text)) {
      this.heard = ''
      return
    }
    this.heard = text
    this.error = null
    this.onSentence(text, out)
  }

  /** A sentence the owner finished: a correction of the last reply, or a message for the chat. */
  private onSentence(text: string, out: LiveEffect[]) {
    // The owner takes back a sentence that is being held by saying so. Other sentences are not taken back.
    if (this.held !== null && CANCEL_START.test(text)) {
      this.held = null
      return
    }
    const found = this.correctionIn(text)
    if (found === null && !this.awaitingCorrection) {
      this.sendOrWait(text, out)
      return
    }
    this.awaitingCorrection = false
    const correction = found ?? text
    if (correction === '') {
      // Only the phrase was said. The next sentence is the correction.
      this.awaitingCorrection = true
      return
    }
    if (this.toSay > 0) this.interruptReply(out)
    if (this.replying) this.queuedCorrection = join(this.queuedCorrection, correction)
    else this.correct(correction, out)
  }

  private onFailed(input: { transient: boolean; message?: string }, out: LiveEffect[]) {
    this.begun = true
    if (!input.transient) {
      this.error = input.message ?? GAVE_UP
      this.end(out)
      return
    }
    this.listening = false
    // A failure while the microphone is off is the recogniser stopping, not a fault to retry.
    if (!this.active || !this.micOn) return
    this.failures++
    if (this.failures > MAX_FAILURES) {
      this.error = GAVE_UP
      this.end(out)
      return
    }
    this.listenDelay = RETRY_MS
  }

  private onSent(started: boolean, out: LiveEffect[]) {
    if (started) {
      this.chatBusy = true
      this.turnStarted = true
      // The owner stopped the turn before the chat began it: stop the reply that has just started.
      if (!this.replying) this.stopChat(out)
      return
    }
    this.replying = false
    this.turnStarted = false
    this.waiting = null
    this.error = NOT_SENT
    const queued = this.queuedCorrection
    this.queuedCorrection = null
    if (queued !== null) this.correct(queued, out)
  }

  private onReply(input: { text: string | null; busy: boolean; error: string | null }, out: LiveEffect[]) {
    this.chatBusy = input.busy
    if (this.stale) {
      // A reply the owner stopped. Its text is ignored until the chat has finished it.
      if (!input.busy) {
        this.stale = false
        this.resume(out)
      }
      return
    }
    if (!this.replying) return
    if (input.busy) {
      this.turnStarted = true
      if (input.text !== null) {
        this.turnReply = input.text
        this.say(this.cutter.cut(input.text), out)
      }
      return
    }
    // The chat has not begun this turn yet: what it holds is the reply from before.
    if (!this.turnStarted) return
    const finished = input.text && input.text !== '' ? input.text : null
    if (finished !== null) this.say(this.cutter.cut(finished, true), out)
    if (input.error !== null) this.error = input.error
    this.lastTurn = finished !== null && finished.trim() !== '' ? { question: this.turnQuestion, reply: finished } : null
    const queued = this.queuedCorrection
    this.queuedCorrection = null
    this.replying = false
    this.turnStarted = false
    this.turnReply = ''
    this.cutter = new SentenceCutter()
    if (queued !== null) this.correct(queued, out)
    this.resume(out)
  }

  private micChanged(on: boolean, out: LiveEffect[]) {
    this.micOn = on
    if (!on) {
      this.heard = ''
      if (this.listening) {
        this.listening = false
        out.push({ kind: 'cancelListening' })
      }
    }
  }

  /** Holds the owner's sentence before it is sent, or queues it while a reply is being written. Talking over a reply stops it. */
  private sendOrWait(text: string, out: LiveEffect[]) {
    if (this.toSay > 0) this.interruptReply(out)
    if (this.replying) {
      this.waiting = join(this.waiting, text)
      return
    }
    this.hold(text, out)
  }

  /** Holds `text` before it is sent. More speech joins the held sentence and extends the hold. */
  private hold(text: string, out: LiveEffect[]) {
    const now = join(this.held, text)
    this.held = now
    out.push({ kind: 'hold', text: now, timed: !this.editBeforeSend })
  }

  /** Sends the held sentence as it now stands. A blank one is dropped. */
  private sendHeld(out: LiveEffect[]) {
    const text = this.held
    if (text === null) return
    this.held = null
    if (text.trim() === '') return
    this.startTurn(text.trim(), out)
  }

  /** Turns edit before send on or off. Turned off, a sentence still held is sent when its timer runs out, as before. */
  private editBeforeSendChanged(on: boolean, out: LiveEffect[]) {
    this.editBeforeSend = on
    const text = this.held
    if (text === null || on) return
    if (text.trim() === '') {
      this.held = null
    } else {
      out.push({ kind: 'hold', text, timed: true })
    }
  }

  private startTurn(text: string, out: LiveEffect[]) {
    this.replying = true
    this.turnStarted = false
    this.turnReply = ''
    this.cutter = new SentenceCutter()
    this.notice = null
    if (this.chatBusy || this.stale) {
      this.waiting = join(this.waiting, text)
    } else {
      this.turnQuestion = text
      out.push({ kind: 'send', text })
    }
  }

  /** Holds what was said while the chat was busy, once the chat is free. */
  private resume(out: LiveEffect[]) {
    const next = this.waiting
    if (next === null) return
    this.waiting = null
    this.hold(next, out)
  }

  /** Records `correction` against the last turn the owner heard. Only called when no reply is being written. */
  private correct(correction: string, out: LiveEffect[]) {
    const turn = this.lastTurn
    if (turn === null) {
      this.say([NO_REPLY], out)
      return
    }
    out.push({ kind: 'record', question: turn.question, answer: turn.reply, correction })
    this.notice = SAVED
    this.say([NOTED], out)
  }

  private say(sentences: string[], out: LiveEffect[]) {
    for (const sentence of sentences) {
      this.toSay++
      this.remember(words(sentence))
      out.push({ kind: 'say', sentence })
    }
  }

  /** Keeps the most recent words the phone has said. */
  private remember(said: string[]) {
    this.spoken.push(...said)
    if (this.spoken.length > SPOKEN_WORDS) this.spoken.splice(0, this.spoken.length - SPOKEN_WORDS)
  }

  /** Stops speech and the chat's reply. What the owner said after that is still heard and sent. */
  private interruptReply(out: LiveEffect[]) {
    if (this.toSay > 0) {
      this.toSay = 0
      out.push({ kind: 'stopSpeech' })
    }
    // What the owner heard of the reply, before talking over it, is the reply a correction refers to.
    if (this.replying && this.turnReply.trim() !== '') this.lastTurn = { question: this.turnQuestion, reply: this.turnReply }
    this.stopChat(out)
    this.replying = false
    this.turnStarted = false
    this.turnReply = ''
    this.cutter = new SentenceCutter()
  }

  private stopChat(out: LiveEffect[]) {
    if (this.chatBusy && !this.stale) {
      this.stale = true
      out.push({ kind: 'stopReply' })
    }
  }

  private end(out: LiveEffect[]) {
    if (this.listening) {
      this.listening = false
      out.push({ kind: 'cancelListening' })
    }
    if (this.toSay > 0) {
      this.toSay = 0
      out.push({ kind: 'stopSpeech' })
    }
    this.stopChat(out)
    this.active = false
    this.replying = false
    this.turnStarted = false
    this.waiting = null
    this.held = null
    this.queuedCorrection = null
    this.awaitingCorrection = false
    this.spoken = []
    this.cutter = new SentenceCutter()
  }

  /** The correction a sentence starts with, or null when it does not start with one. */
  private correctionIn(text: string): string | null {
    const m = CORRECTION_START.exec(text)
    if (!m) return null
    return text.slice(m[0].length).trim()
  }

  /**
   * Marks each word of `all` that is part of a pair of words the phone has just said, one after the other. A pair is the
   * smallest unit that counts, so one common word such as "you" does not mark anything by itself.
   */
  private echoed(all: string[]): boolean[] {
    const marks = all.map(() => false)
    if (this.spoken.length < 2) return marks
    const said = new Set<string>()
    for (let i = 0; i + 1 < this.spoken.length; i++) said.add(`${this.spoken[i]} ${this.spoken[i + 1]}`)
    for (let i = 0; i + 1 < all.length; i++) {
      if (said.has(`${all[i]} ${all[i + 1]}`)) {
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
  private echoLike(text: string): boolean {
    const all = words(text)
    if (all.length === 0) return true
    const share = this.echoed(all).filter((m) => m).length / all.length
    return share >= ECHO_SHARE || (all.length >= ECHO_LONG_WORDS && share >= ECHO_LONG_SHARE)
  }

  /** True when the owner is talking over the phone: at least two words that are not the phone's own voice. */
  private interrupts(text: string): boolean {
    const all = words(text)
    const own = this.echoed(all)
    return own.filter((m) => !m).length >= 2 && !this.echoLike(text)
  }
}
