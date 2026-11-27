import { describe, expect, it } from 'vitest'
import { GAVE_UP, LiveTurns, NOT_SENT, NO_REPLY, NOTED, SAVED, type LiveEffect, type LiveInput } from './turns'

// Port of the Android LiveTurnsTest: the same conversations, with the same effects expected.
const start = (): LiveInput => ({ kind: 'start' })
const said = (text: string): LiveInput => ({ kind: 'said', text })
const heard = (text: string): LiveInput => ({ kind: 'heard', text })
const sent = (started: boolean): LiveInput => ({ kind: 'sent', started })
const reply = (text: string | null, busy: boolean, error: string | null = null): LiveInput => ({ kind: 'reply', text, busy, error })
const failed = (transient: boolean, message?: string): LiveInput => ({ kind: 'failed', transient, message })
const mic = (on: boolean): LiveInput => ({ kind: 'mic', on })
const editMode = (on: boolean): LiveInput => ({ kind: 'editBeforeSend', on })
const editHeld = (text: string): LiveInput => ({ kind: 'editHeld', text })
const SPOKEN: LiveInput = { kind: 'spoken' }
const HOLD_ENDED: LiveInput = { kind: 'holdEnded' }
const SEND_HELD: LiveInput = { kind: 'sendHeld' }
const CANCEL_HOLD: LiveInput = { kind: 'cancelHold' }
const INTERRUPT: LiveInput = { kind: 'interrupt' }
const END: LiveInput = { kind: 'end' }

const listen = (delayMs = 0): LiveEffect => ({ kind: 'listen', delayMs })
const hold = (text: string, timed = true): LiveEffect => ({ kind: 'hold', text, timed })
const send = (text: string): LiveEffect => ({ kind: 'send', text })
const says = (sentence: string): LiveEffect => ({ kind: 'say', sentence })
const record = (question: string, answer: string, correction: string): LiveEffect => ({ kind: 'record', question, answer, correction })
const STOP_SPEECH: LiveEffect = { kind: 'stopSpeech' }
const STOP_REPLY: LiveEffect = { kind: 'stopReply' }
const CANCEL_LISTENING: LiveEffect = { kind: 'cancelListening' }

describe('Live turns', () => {
  let turns: LiveTurns
  /** The owner's sentence, held and then sent, for tests that are not about the hold. */
  const sendSaid = (text: string) => {
    turns.handle(said(text))
    turns.handle(HOLD_ENDED)
  }
  const fresh = () => {
    turns = new LiveTurns()
  }

  it('starts listening and shows listening', () => {
    fresh()
    expect(turns.phase).toBe('starting')
    expect(turns.handle(start())).toEqual([listen(0)])
    expect(turns.phase).toBe('listening')
  })

  it('holds a finished sentence, then sends it when the hold ends', () => {
    fresh()
    turns.handle(start())
    expect(turns.handle(said('what time is it'))).toEqual([hold('what time is it'), listen(0)])
    expect(turns.held).toBe('what time is it')
    expect(turns.handle(HOLD_ENDED)).toEqual([send('what time is it')])
    expect(turns.held).toBeNull()
    expect(turns.phase).toBe('thinking')
  })

  it('does not send a sentence that was cancelled', () => {
    fresh()
    turns.handle(start())
    turns.handle(said('the tv says so'))
    expect(turns.handle(CANCEL_HOLD)).toEqual([])
    expect(turns.held).toBeNull()
    expect(turns.handle(HOLD_ENDED)).toEqual([])
    expect(turns.phase).toBe('listening')
  })

  it('joins more speech to a sentence that is held', () => {
    fresh()
    turns.handle(start())
    turns.handle(said('what time'))
    expect(turns.handle(said('is it'))).toEqual([hold('what time is it'), listen(0)])
    expect(turns.handle(HOLD_ENDED)).toEqual([send('what time is it')])
  })

  it('takes back the held sentence when the owner says cancel', () => {
    fresh()
    turns.handle(start())
    turns.handle(said('the news is on'))
    expect(turns.handle(said('cancel that'))).toEqual([listen(0)])
    expect(turns.held).toBeNull()
    expect(turns.handle(HOLD_ENDED)).toEqual([])
  })

  it('drops a held sentence when the owner interrupts', () => {
    fresh()
    turns.handle(start())
    turns.handle(said('hold this'))
    expect(turns.handle(INTERRUPT)).toEqual([])
    expect(turns.held).toBeNull()
    expect(turns.handle(HOLD_ENDED)).toEqual([])
  })

  it('says a reply sentence by sentence, and keeps listening', () => {
    fresh()
    turns.handle(start())
    sendSaid('what time is it')
    expect(turns.handle(sent(true))).toEqual([])
    expect(turns.handle(reply('It is noon. Anything else?', true))).toEqual([says('It is noon.')])
    expect(turns.phase).toBe('speaking')
    expect(turns.handle(reply('It is noon. Anything else?', false))).toEqual([says('Anything else?')])
    turns.handle(SPOKEN)
    expect(turns.phase).toBe('speaking')
    turns.handle(SPOKEN)
    expect(turns.phase).toBe('listening')
  })

  it('starts listening again quickly after silence', () => {
    fresh()
    turns.handle(start())
    expect(turns.handle(said(''))).toEqual([listen(300)])
    expect(turns.phase).toBe('listening')
  })

  it('retries transient failures, then gives up', () => {
    fresh()
    turns.handle(start())
    expect(turns.handle(failed(true))).toEqual([listen(800)])
    for (let i = 0; i < 3; i++) turns.handle(failed(true))
    const last = turns.handle(failed(true))
    expect(turns.phase).toBe('stopped')
    expect(turns.error).toBe(GAVE_UP)
    expect(last.some((e) => e.kind === 'listen')).toBe(false)
  })

  it('ends the conversation with the reason for a permanent failure', () => {
    fresh()
    turns.handle(start())
    const effects = turns.handle(failed(false, 'The microphone is not allowed.'))
    expect(turns.phase).toBe('stopped')
    expect(turns.error).toBe('The microphone is not allowed.')
    expect(effects).toEqual([CANCEL_LISTENING])
  })

  it('stops the reply when the owner talks over it, and sends what is said', () => {
    fresh()
    turns.handle(start())
    sendSaid('tell me a story')
    turns.handle(sent(true))
    turns.handle(reply('Once upon a time. The end.', true))
    expect(turns.phase).toBe('speaking')

    expect(turns.handle(heard('wait stop that'))).toEqual([STOP_SPEECH, STOP_REPLY])
    expect(turns.phase).toBe('listening')

    // The chat's old reply keeps arriving until it has stopped. None of it is spoken.
    expect(turns.handle(reply('Once upon a time. The end.', true))).toEqual([])
    expect(turns.handle(reply('Once upon a time. The end.', false))).toEqual([])

    expect(turns.handle(said('wait stop that'))).toEqual([hold('wait stop that'), listen(0)])
    expect(turns.handle(HOLD_ENDED)).toEqual([send('wait stop that')])
  })

  it('does not take what the phone says itself as the owner talking', () => {
    fresh()
    turns.handle(start())
    sendSaid('hello')
    turns.handle(sent(true))
    turns.handle(reply('Hello there. How can I help you today?', true))
    expect(turns.phase).toBe('speaking')
    // The recogniser hears the sentence being said. Nothing is interrupted and nothing is sent.
    expect(turns.handle(said('hello there'))).toEqual([listen(0)])
    expect(turns.handle(heard('hello there'))).toEqual([])
    expect(turns.phase).toBe('speaking')
  })

  it('holds a sentence said while the chat is writing until its turn comes', () => {
    fresh()
    turns.handle(start())
    sendSaid('first question')
    turns.handle(sent(true))
    turns.handle(reply('', true))
    expect(turns.handle(said('and a second one'))).toEqual([listen(0)])
    expect(turns.phase).toBe('thinking')
    expect(turns.handle(reply('Answer one.', false))).toEqual([says('Answer one.'), hold('and a second one')])
    expect(turns.handle(HOLD_ENDED)).toEqual([send('and a second one')])
  })

  it('stops the reply on interrupt and drops what was queued', () => {
    fresh()
    turns.handle(start())
    sendSaid('tell me')
    turns.handle(sent(true))
    turns.handle(reply('', true))
    turns.handle(said('and this too'))
    expect(turns.handle(INTERRUPT)).toEqual([STOP_REPLY])
    expect(turns.phase).toBe('listening')
    expect(turns.handle(reply('', false))).toEqual([])
    expect(turns.handle(said('new question'))).toEqual([hold('new question'), listen(0)])
    expect(turns.handle(HOLD_ENDED)).toEqual([send('new question')])
  })

  it('reports a message the chat refused, and goes on listening', () => {
    fresh()
    turns.handle(start())
    sendSaid('hello')
    expect(turns.handle(sent(false))).toEqual([])
    expect(turns.phase).toBe('listening')
    expect(turns.error).toBe(NOT_SENT)
  })

  it('does not say an old reply again', () => {
    fresh()
    turns.handle(start())
    expect(turns.handle(reply('Old answer.', false))).toEqual([])
  })

  it('returns to listening when a reply fails', () => {
    fresh()
    turns.handle(start())
    sendSaid('hi')
    turns.handle(sent(true))
    turns.handle(reply('', true))
    turns.handle(reply(null, false, 'The model stopped.'))
    expect(turns.phase).toBe('listening')
    expect(turns.error).toBe('The model stopped.')
  })

  it('stops everything at the end', () => {
    fresh()
    turns.handle(start())
    sendSaid('hi')
    turns.handle(sent(true))
    turns.handle(reply('Hello. There', true))
    expect(turns.handle(END)).toEqual([CANCEL_LISTENING, STOP_SPEECH, STOP_REPLY])
    expect(turns.phase).toBe('stopped')
  })

  it('stops listening with the microphone off, and starts again when it is on', () => {
    fresh()
    turns.handle(start())
    expect(turns.handle(mic(false))).toEqual([CANCEL_LISTENING])
    expect(turns.micOn).toBe(false)
    // While the microphone is off, nothing is taken and the phone does not restart by itself.
    expect(turns.handle(said('hello'))).toEqual([])
    expect(turns.handle(failed(true))).toEqual([])
    expect(turns.handle(mic(true))).toEqual([listen(0)])
    expect(turns.micOn).toBe(true)
  })

  it('keeps a reply playing while the microphone is off', () => {
    fresh()
    turns.handle(start())
    sendSaid('tell me')
    turns.handle(sent(true))
    expect(turns.handle(mic(false))).toEqual([CANCEL_LISTENING])
    expect(turns.handle(reply('Here it is. More.', true))).toEqual([says('Here it is.')])
    expect(turns.handle(reply('Here it is. More.', false))).toEqual([says('More.')])
    turns.handle(SPOKEN)
    turns.handle(SPOKEN)
    expect(turns.phase).toBe('listening')
    expect(turns.handle(mic(true))).toEqual([listen(0)])
  })

  it('records a correction in the same sentence against the last reply', () => {
    fresh()
    turns.handle(start())
    sendSaid('what is the capital of spain')
    turns.handle(sent(true))
    turns.handle(reply('Lyon is the capital.', false))
    turns.handle(SPOKEN)
    expect(turns.handle(said("that's wrong, it is Madrid"))).toEqual([
      record('what is the capital of spain', 'Lyon is the capital.', 'it is Madrid'),
      says(NOTED),
      listen(0),
    ])
    expect(turns.notice).toBe(SAVED)
  })

  it('takes the next sentence as the correction when the phrase comes on its own', () => {
    fresh()
    turns.handle(start())
    sendSaid('what is the capital of spain')
    turns.handle(sent(true))
    turns.handle(reply('Lyon is the capital.', false))
    turns.handle(SPOKEN)
    expect(turns.handle(said("that's wrong"))).toEqual([listen(0)])
    expect(turns.awaitingCorrection).toBe(true)
    expect(turns.handle(said('it is Madrid'))).toEqual([
      record('what is the capital of spain', 'Lyon is the capital.', 'it is Madrid'),
      says(NOTED),
      listen(0),
    ])
    expect(turns.awaitingCorrection).toBe(false)
  })

  it('says there is no reply to correct yet, and records nothing', () => {
    fresh()
    turns.handle(start())
    expect(turns.handle(said('correction it is a cat'))).toEqual([says(NO_REPLY), listen(0)])
  })

  it('records a correction said while the reply is written when the reply finishes', () => {
    fresh()
    turns.handle(start())
    sendSaid('tell me')
    turns.handle(sent(true))
    turns.handle(reply('', true))
    expect(turns.handle(said('correction it is a cat'))).toEqual([listen(0)])
    expect(turns.handle(reply('It is a dog.', false))).toEqual([
      says('It is a dog.'),
      record('tell me', 'It is a dog.', 'it is a cat'),
      says(NOTED),
    ])
  })

  it('records what was heard when the owner talks over a reply with a correction', () => {
    fresh()
    turns.handle(start())
    sendSaid('tell me a fable')
    turns.handle(sent(true))
    turns.handle(reply('Once upon a time. The end.', true))
    expect(turns.handle(heard("that's wrong it's a fable"))).toEqual([STOP_SPEECH, STOP_REPLY])
    // A correction is recorded, not held and sent to the chat.
    expect(turns.handle(said("that's wrong it's a fable"))).toEqual([
      record('tell me a fable', 'Once upon a time. The end.', "it's a fable"),
      says(NOTED),
      listen(0),
    ])
  })

  it('keeps a phrase in the middle of a sentence as a message', () => {
    fresh()
    turns.handle(start())
    expect(turns.handle(said('it is not quite what I wanted'))).toEqual([hold('it is not quite what I wanted'), listen(0)])
    expect(turns.handle(HOLD_ENDED)).toEqual([send('it is not quite what I wanted')])
  })

  it('drops a waiting correction on interrupt', () => {
    fresh()
    turns.handle(start())
    turns.handle(said("that's wrong"))
    expect(turns.awaitingCorrection).toBe(true)
    turns.handle(INTERRUPT)
    expect(turns.awaitingCorrection).toBe(false)
  })

  it("does not take the echo of a reply heard after it ends as the owner's talk", () => {
    fresh()
    turns.handle(start())
    sendSaid('how are you')
    turns.handle(sent(true))
    expect(turns.handle(reply('Doing well, thanks for asking. How about you?', true))).toEqual([says('Doing well, thanks for asking.')])
    expect(turns.handle(reply('Doing well, thanks for asking. How about you?', false))).toEqual([says('How about you?')])
    turns.handle(SPOKEN)
    turns.handle(SPOKEN)
    expect(turns.phase).toBe('listening')
    // The recogniser's result for the phone's own sentence arrives after it has finished speaking.
    expect(turns.handle(said('doing well thanks for asking how about you'))).toEqual([listen(0)])
    expect(turns.heard).toBe('')
    expect(turns.held).toBeNull()
    expect(turns.phase).toBe('listening')
  })

  it('still takes a long echo with one recognition error as the echo', () => {
    fresh()
    turns.handle(start())
    sendSaid('are you there')
    turns.handle(sent(true))
    turns.handle(reply('Let me think about that for a moment.', false))
    expect(turns.handle(said('let me thinks about that for a moment'))).toEqual([listen(0)])
    expect(turns.heard).toBe('')
    expect(turns.held).toBeNull()
  })

  it('still sends real speech that shares some words with the reply', () => {
    fresh()
    turns.handle(start())
    sendSaid('what do you think')
    turns.handle(sent(true))
    turns.handle(reply('That is a good idea.', false))
    turns.handle(SPOKEN)
    expect(turns.handle(said('is it a good idea to go out'))).toEqual([hold('is it a good idea to go out'), listen(0)])
    expect(turns.heard).toBe('is it a good idea to go out')
  })

  it("does not show a part of the phone's own voice as the owner's words", () => {
    fresh()
    turns.handle(start())
    sendSaid('hi')
    turns.handle(sent(true))
    turns.handle(reply('Hello there. How can I help?', true))
    turns.handle(heard('hello there'))
    expect(turns.heard).toBe('')
    expect(turns.phase).toBe('speaking')
  })

  describe('edit before send', () => {
    it('keeps a held sentence until the owner sends it', () => {
      fresh()
      turns.handle(start())
      turns.handle(editMode(true))
      expect(turns.editBeforeSend).toBe(true)
      expect(turns.handle(said('what time is it'))).toEqual([hold('what time is it', false), listen(0)])
      expect(turns.handle(HOLD_ENDED)).toEqual([])
      expect(turns.held).toBe('what time is it')
      expect(turns.handle(SEND_HELD)).toEqual([send('what time is it')])
      expect(turns.held).toBeNull()
    })

    it('lets the owner change the held sentence before sending it', () => {
      fresh()
      turns.handle(start())
      turns.handle(editMode(true))
      turns.handle(said('what time is it'))
      expect(turns.handle(editHeld('what is the time'))).toEqual([])
      expect(turns.held).toBe('what is the time')
      expect(turns.handle(SEND_HELD)).toEqual([send('what is the time')])
    })

    it('joins speech to the held sentence in edit mode', () => {
      fresh()
      turns.handle(start())
      turns.handle(editMode(true))
      turns.handle(said('what time'))
      expect(turns.handle(said('is it'))).toEqual([hold('what time is it', false), listen(0)])
      expect(turns.held).toBe('what time is it')
    })

    it('starts the hold timer again when edit mode is turned off', () => {
      fresh()
      turns.handle(start())
      turns.handle(editMode(true))
      turns.handle(said('what time is it'))
      expect(turns.handle(editMode(false))).toEqual([hold('what time is it', true)])
      expect(turns.handle(HOLD_ENDED)).toEqual([send('what time is it')])
    })

    it('does not send a held sentence that was cleared', () => {
      fresh()
      turns.handle(start())
      turns.handle(editMode(true))
      turns.handle(said('what time is it'))
      turns.handle(editHeld(''))
      expect(turns.handle(SEND_HELD)).toEqual([])
      expect(turns.held).toBeNull()
    })

    it('drops the held sentence on cancel send, even in edit mode', () => {
      fresh()
      turns.handle(start())
      turns.handle(editMode(true))
      turns.handle(said('the news is on'))
      turns.handle(CANCEL_HOLD)
      expect(turns.held).toBeNull()
      expect(turns.handle(SEND_HELD)).toEqual([])
    })
  })
})
