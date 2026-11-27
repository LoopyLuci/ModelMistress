import { describe, expect, it } from 'vitest'
import { SentenceCutter, cleanSentence, withoutThinking } from './cutter'

describe('cutting a reply into sentences', () => {
  it('says each sentence once, as its end arrives', () => {
    const c = new SentenceCutter()
    expect(c.cut('It is noon')).toEqual([])
    expect(c.cut('It is noon. Anything')).toEqual(['It is noon.'])
    expect(c.cut('It is noon. Anything else? ')).toEqual(['Anything else?'])
    expect(c.cut('It is noon. Anything else? ')).toEqual([])
  })

  it('says the unfinished last piece only when the reply is final', () => {
    const c = new SentenceCutter()
    expect(c.cut('Done. And then')).toEqual(['Done.'])
    expect(c.cut('Done. And then', true)).toEqual(['And then'])
  })

  it('does not cut a number with a full stop in it', () => {
    expect(new SentenceCutter().cut('Pi is 3.14 roughly. Next', false)).toEqual(['Pi is 3.14 roughly.'])
    expect(new SentenceCutter().cut('Pi is 3.14', true)).toEqual(['Pi is 3.14'])
  })

  it('cuts at a line break', () => {
    expect(new SentenceCutter().cut('First line\nSecond line')).toEqual(['First line'])
  })

  it('never says text inside think tags, and hides an unfinished one', () => {
    expect(withoutThinking('<think>secret. more</think>Hello.')).toBe('Hello.')
    expect(withoutThinking('Hello. <think>still thinking. ')).toBe('Hello. ')
    expect(new SentenceCutter().cut('<think>hidden. text</think>Shown.', true)).toEqual(['Shown.'])
  })

  it('leaves out markdown marks and collapses spaces', () => {
    expect(cleanSentence('**Bold**   and `code`.')).toBe('Bold and code.')
    expect(cleanSentence('* # ~~ __')).toBeNull()
  })

  it('starts again when the reply shrinks', () => {
    const c = new SentenceCutter()
    expect(c.cut('One. Two. Three. Four.')).toEqual(['One.', 'Two.', 'Three.'])
    // a tool step cleared the text: the new, shorter reply is cut from its beginning
    expect(c.cut('Ok. Go.')).toEqual(['Ok.'])
  })
})
