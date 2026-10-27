import { describe, expect, it } from 'vitest'
import { chats, newConversation, removeConversation, sorted } from './store'

describe('conversations', () => {
  it('starts a new chat for a model and keeps the newest first', () => {
    const first = newConversation('qwen3-8b')
    const second = newConversation('')

    expect(first.title).toBe('New chat')
    expect(first.model).toBe('qwen3-8b')
    expect(chats.list[0].id).toBe(second.id)
    expect(sorted()[0].id).toBe(second.id)
  })

  it('copies the settings of the chat it is made from', () => {
    const source = newConversation('qwen3-8b')
    source.system = 'Be brief.'
    source.params = { temperature: 0.2, top_p: 0.9, max_tokens: 64 }

    const copy = newConversation('', source)

    expect(copy.model).toBe('qwen3-8b')
    expect(copy.system).toBe('Be brief.')
    expect(copy.params).toEqual({ temperature: 0.2, top_p: 0.9, max_tokens: 64 })
    expect(copy.params).not.toBe(source.params)
  })

  it('removes a conversation by id', () => {
    const c = newConversation('m')
    const before = chats.list.length

    removeConversation(c.id)

    expect(chats.list.length).toBe(before - 1)
    expect(chats.list.find((x) => x.id === c.id)).toBeUndefined()
  })
})
