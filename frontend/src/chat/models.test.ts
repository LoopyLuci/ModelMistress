import { describe, expect, it } from 'vitest'
import { groupModels, isChatModel, modelLabel, type HubModel } from './models'

const model = (over: Partial<HubModel>): HubModel => ({ id: 'm', source: 'hf', ...over })

describe('isChatModel', () => {
  it('accepts a plain language model', () => {
    expect(isChatModel(model({ id: 'qwen3-8b' }))).toBe(true)
  })

  it('refuses speech, vision, image and embedding models', () => {
    expect(isChatModel(model({ id: 'base', source: 'whisper' }))).toBe(false)
    expect(isChatModel(model({ id: 'base', source: 'vision' }))).toBe(false)
    expect(isChatModel(model({ id: 'x', architecture: 'flux' }))).toBe(false)
    expect(isChatModel(model({ id: 'x', kind: 'image' }))).toBe(false)
    expect(isChatModel(model({ id: 'nomic-embed-text' }))).toBe(false)
  })
})

describe('groupModels', () => {
  it('puts MM models first, then local and Ollama models, and drops empty groups', () => {
    const groups = groupModels([
      model({ id: 'b', source: 'hf' }),
      model({ id: 'a', source: 'ollama:llama' }),
      model({ id: 'dusty', source: 'native', native: true }),
    ])
    expect(groups.map((g) => g.label)).toEqual(['MM models', 'Local models', 'Ollama'])
    expect(groups[1].models.map((m) => m.id)).toEqual(['b'])
    expect(groupModels([])).toEqual([])
  })
})

describe('modelLabel', () => {
  it('adds the size and the loaded state when they are known', () => {
    expect(modelLabel(model({ id: 'm', size_gb: 4.5, loaded: true }))).toBe('m (4.5 GB) - loaded')
    expect(modelLabel(model({ id: 'm' }))).toBe('m')
  })
})
