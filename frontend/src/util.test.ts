import { describe, expect, it } from 'vitest'
import { diffLines, fmtBytes, fmtNum, recordText, splitList } from './util'

describe('fmtBytes', () => {
  it('shows a dash for nothing and plain bytes below a kibibyte', () => {
    expect(fmtBytes(null)).toBe('-')
    expect(fmtBytes(undefined)).toBe('-')
    expect(fmtBytes(512)).toBe('512 B')
  })

  it('picks the unit and the number of decimals from the size', () => {
    expect(fmtBytes(1536)).toBe('1.50 KiB')
    expect(fmtBytes(12 * 1024 ** 3)).toBe('12.0 GiB')
  })
})

describe('fmtNum', () => {
  it('rounds to three decimals by default and shows a dash for missing numbers', () => {
    expect(fmtNum(1.23456)).toBe('1.235')
    expect(fmtNum(2, 1)).toBe('2.0')
    expect(fmtNum(null)).toBe('-')
    expect(fmtNum(NaN)).toBe('-')
  })
})

describe('splitList', () => {
  it('splits on commas, trims and drops empty items', () => {
    expect(splitList(' a, ,b ,, c ')).toEqual(['a', 'b', 'c'])
    expect(splitList('')).toEqual([])
  })
})

describe('diffLines', () => {
  it('keeps the common lines and marks the changed ones as deleted and inserted', () => {
    expect(diffLines('a\nb\nc', 'a\nx\nc')).toEqual([
      { op: 'same', text: 'a' },
      { op: 'del', text: 'b' },
      { op: 'ins', text: 'x' },
      { op: 'same', text: 'c' },
    ])
  })
})

describe('recordText', () => {
  it('drops the revision bookkeeping and pretty-prints the rest', () => {
    expect(recordText({ rev: 3, edited_by: 'me', id: 'r1' })).toBe('{\n  "id": "r1"\n}')
    expect(recordText(null)).toBe('')
  })
})
