// Message history in a message box: Up and Down walk back and forth through what was sent, like a shell does.
//
//   Up    on an empty box, or with the caret at the very start of the text: the message sent before. Pressed again
//         it goes further back. A draft being typed is kept and comes back when Down walks past the newest message.
//   Down  while walking: the next newer message, and at the end the draft.
//
// What Up walks through is, oldest first, what was sent in other chats and then what was sent in this one, so a new
// chat can still repeat or fix the last thing sent anywhere. Keys inside a multi-line message move the caret as
// usual: history takes over only when the caret is on the first line (Up) or the last line (Down).
import { nextTick } from 'vue'

const KEY = 'mm_input_history'
/** How many sent messages are kept between visits. */
export const KEEP = 200

/** Messages sent from any chat or Misty session, oldest first. */
export function loadSent(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) || '[]')
    return Array.isArray(v) ? v.filter((x) => typeof x === 'string') : []
  } catch (_) {
    return []
  }
}

/** Keeps a sent message for the next visit (a repeat of the newest one is not kept twice). */
export function remember(text: string): void {
  const t = text.trim()
  if (!t) return
  const list = loadSent()
  if (list[list.length - 1] === t) return
  list.push(t)
  try {
    localStorage.setItem(KEY, JSON.stringify(list.slice(-KEEP)))
  } catch (_) {
    /* storage full or blocked: history is a convenience */
  }
}

/** Oldest first: what was sent elsewhere, then what was sent in this chat. A message repeated in a row shows once. */
export function entriesFor(own: string[], sent: string[]): string[] {
  const mine = own.map((t) => t.trim()).filter(Boolean)
  const here = new Set(mine)
  const out: string[] = []
  for (const t of [...sent.filter((x) => !here.has(x)), ...mine]) {
    if (out[out.length - 1] !== t) out.push(t)
  }
  return out
}

export interface Recall {
  /** Is the box showing a recalled message (or the walk's way back to the draft)? */
  readonly walking: boolean
  /** The message before the one shown; `current` is the box's text, kept as the draft when the walk starts. `null`: none. */
  older(current: string): string | null
  /** The next newer message, then the draft; `null` when not walking. */
  newer(): string | null
  /** The box was changed by hand: what it holds is a new draft and the walk starts over. */
  edited(current: string): void
  /** Sent or cleared. */
  reset(): void
}

export function createRecall(entries: () => string[]): Recall {
  let pos: number | null = null
  let draft = ''
  return {
    get walking() {
      return pos !== null
    },
    older(current) {
      const list = entries()
      if (!list.length) return null
      if (pos === null) {
        draft = current
        pos = list.length
      } else if (pos > list.length) {
        pos = list.length // the list got shorter while walking
      }
      if (pos === 0) return null
      pos--
      return list[pos]
    },
    newer() {
      if (pos === null) return null
      const list = entries()
      pos++
      if (pos >= list.length) {
        pos = null
        return draft
      }
      return list[pos]
    },
    edited(current) {
      if (pos !== null && current !== entries()[pos]) pos = null
    },
    reset() {
      pos = null
      draft = ''
    },
  }
}

export type Intent = 'older' | 'newer' | null

interface KeyLike {
  key: string
  shiftKey?: boolean
  altKey?: boolean
  ctrlKey?: boolean
  metaKey?: boolean
  isComposing?: boolean
}

/** What a key press means for history, given the box's text and caret. Pure, so the rules are tested without a browser. */
export function intentOf(e: KeyLike, value: string, start: number, end: number, walking: boolean): Intent {
  if (e.shiftKey || e.altKey || e.ctrlKey || e.metaKey || e.isComposing) return null
  if (start !== end) return null // a selection: the arrow collapses it
  if (e.key === 'ArrowUp') {
    const onFirstLine = !value.slice(0, start).includes('\n')
    return value === '' || start === 0 || (walking && onFirstLine) ? 'older' : null
  }
  if (e.key === 'ArrowDown' && walking) {
    return value.slice(end).includes('\n') ? null : 'newer'
  }
  return null
}

/**
 * Handles an Up or Down press in `el`: puts the recalled text in the box through `set` (the v-model) with the caret at
 * its end. Returns true when it did, and the key must not do anything else.
 */
export function recallKey(e: KeyboardEvent, el: HTMLTextAreaElement, recall: Recall, set: (text: string) => void): boolean {
  const intent = intentOf(e, el.value, el.selectionStart, el.selectionEnd, recall.walking)
  if (!intent) return false
  const next = intent === 'older' ? recall.older(el.value) : recall.newer()
  if (next === null) return false
  e.preventDefault()
  set(next)
  nextTick(() => el.setSelectionRange(next.length, next.length))
  return true
}
