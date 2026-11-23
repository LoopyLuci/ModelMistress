import { beforeEach, describe, expect, it } from 'vitest'
import { KEEP, createRecall, entriesFor, intentOf, loadSent, remember } from './recall'

const key = (k: string, extra: Record<string, boolean> = {}) => ({ key: k, ...extra })

describe('what Up walks through', () => {
  it('lists other chats first and this chat last, oldest first, without repeats in a row', () => {
    expect(entriesFor(['b', 'c'], ['a', 'x'])).toEqual(['a', 'x', 'b', 'c'])
    // a message sent in this chat is not also listed with the others
    expect(entriesFor(['a', 'b'], ['a', 'x'])).toEqual(['x', 'a', 'b'])
    expect(entriesFor(['go', 'go', 'next'], [])).toEqual(['go', 'next'])
    expect(entriesFor(['  hi  ', ' '], [])).toEqual(['hi'])
    expect(entriesFor([], [])).toEqual([])
  })
})

describe('walking back and forth', () => {
  it('goes to older messages, stops at the oldest, and comes back to the draft', () => {
    const r = createRecall(() => ['one', 'two', 'three'])
    expect(r.walking).toBe(false)
    expect(r.older('my draft')).toBe('three')
    expect(r.walking).toBe(true)
    expect(r.older('three')).toBe('two')
    expect(r.older('two')).toBe('one')
    expect(r.older('one')).toBeNull() // nothing older: the box stays
    expect(r.newer()).toBe('two')
    expect(r.newer()).toBe('three')
    expect(r.newer()).toBe('my draft') // past the newest: the draft is back
    expect(r.walking).toBe(false)
    expect(r.newer()).toBeNull()
  })

  it('does nothing when nothing was ever sent', () => {
    const r = createRecall(() => [])
    expect(r.older('typing')).toBeNull()
    expect(r.walking).toBe(false)
  })

  it('treats an edit of a recalled message as a new draft and starts over from the newest', () => {
    const r = createRecall(() => ['one', 'two'])
    r.older('')
    r.older('two')
    r.edited('one') // the box still shows what was recalled: still walking
    expect(r.walking).toBe(true)
    r.edited('one, changed')
    expect(r.walking).toBe(false)
    expect(r.older('one, changed')).toBe('two')
    r.reset()
    expect(r.walking).toBe(false)
    expect(r.older('')).toBe('two')
  })

  it('keeps the draft that was typed when the walk began, not the recalled texts', () => {
    const r = createRecall(() => ['one', 'two'])
    r.older('half a thought')
    r.older('two')
    r.newer()
    expect(r.newer()).toBe('half a thought')
  })

  it('copes with a list that got shorter while walking', () => {
    let list = ['a', 'b', 'c']
    const r = createRecall(() => list)
    r.older('')
    r.older('c')
    list = ['a']
    expect(r.older('x')).toBe('a') // starts again from the end of the shorter list
  })
})

describe('which key presses are history', () => {
  it('Up recalls on an empty box and at the very start of the text', () => {
    expect(intentOf(key('ArrowUp'), '', 0, 0, false)).toBe('older')
    expect(intentOf(key('ArrowUp'), 'draft', 0, 0, false)).toBe('older')
  })

  it('Up in the middle or at the end of typed text moves the caret as usual', () => {
    expect(intentOf(key('ArrowUp'), 'draft', 5, 5, false)).toBeNull()
    expect(intentOf(key('ArrowUp'), 'draft', 2, 2, false)).toBeNull()
  })

  it('while walking Up goes back from anywhere on the first line, but inside a multi-line message it moves the caret', () => {
    expect(intentOf(key('ArrowUp'), 'a recalled line', 15, 15, true)).toBe('older')
    expect(intentOf(key('ArrowUp'), 'line one\nline two', 17, 17, true)).toBeNull() // on line two
    expect(intentOf(key('ArrowUp'), 'line one\nline two', 4, 4, true)).toBe('older') // on line one
    expect(intentOf(key('ArrowUp'), '\nsecond', 0, 0, false)).toBe('older')
  })

  it('Down is history only while walking and only on the last line', () => {
    expect(intentOf(key('ArrowDown'), 'a recalled line', 0, 0, true)).toBe('newer')
    expect(intentOf(key('ArrowDown'), 'line one\nline two', 3, 3, true)).toBeNull()
    expect(intentOf(key('ArrowDown'), 'line one\nline two', 12, 12, true)).toBe('newer')
    expect(intentOf(key('ArrowDown'), 'typing', 6, 6, false)).toBeNull()
  })

  it('leaves a selection, other keys and modified or composing arrows alone', () => {
    expect(intentOf(key('ArrowUp'), 'text', 0, 4, false)).toBeNull()
    expect(intentOf(key('Enter'), '', 0, 0, true)).toBeNull()
    for (const m of ['shiftKey', 'altKey', 'ctrlKey', 'metaKey', 'isComposing']) {
      expect(intentOf(key('ArrowUp', { [m]: true }), '', 0, 0, true), m).toBeNull()
      expect(intentOf(key('ArrowDown', { [m]: true }), 'x', 1, 1, true), m).toBeNull()
    }
  })
})

describe('what is kept between visits', () => {
  beforeEach(() => localStorage.clear())

  it('keeps sent messages in order, skips blanks and an immediate repeat, and keeps the newest', () => {
    remember('first')
    remember('  ')
    remember('second')
    remember('second')
    remember('  padded ')
    expect(loadSent()).toEqual(['first', 'second', 'padded'])
    for (let i = 0; i < KEEP + 20; i++) remember(`m${i}`)
    const kept = loadSent()
    expect(kept).toHaveLength(KEEP)
    expect(kept[kept.length - 1]).toBe(`m${KEEP + 19}`)
  })

  it('reads nothing from damaged storage and does not fail when storage refuses', () => {
    localStorage.setItem('mm_input_history', '{not json')
    expect(loadSent()).toEqual([])
    localStorage.setItem('mm_input_history', JSON.stringify(['ok', 5, null]))
    expect(loadSent()).toEqual(['ok'])
    const real = Storage.prototype.setItem
    Storage.prototype.setItem = () => {
      throw new Error('full')
    }
    try {
      expect(() => remember('x')).not.toThrow()
    } finally {
      Storage.prototype.setItem = real
    }
  })
})
