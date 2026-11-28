import { describe, expect, it } from 'vitest'
import { allowedLabel, newBot, parseChats, SERVICES, type BotSetting } from './bots'

describe('bots', () => {
  it('reads chat ids typed with commas, spaces or new lines', () => {
    expect(parseChats('42, -100123\n 7  ')).toEqual(['42', '-100123', '7'])
    expect(parseChats('   ')).toEqual([])
  })

  it('makes a new bot off, with no chat allowed, under a name no other bot has', () => {
    const first = newBot([])
    expect(first).toMatchObject({ name: 'telegram-1', kind: 'telegram', enabled: false, allowed_chats: [] })
    const existing: BotSetting[] = [{ ...first, token: '(set)' }, { ...first, name: 'telegram-2' }]
    expect(newBot(existing).name).toBe('telegram-3')
  })

  it('names the allowed ids after the service: chats for Telegram, users for Discord', () => {
    expect(allowedLabel('telegram')).toContain('Telegram chat ids')
    expect(allowedLabel('discord')).toContain('Discord user ids')
    expect(SERVICES.map((s) => s.id)).toEqual(['telegram', 'discord'])
  })
})
