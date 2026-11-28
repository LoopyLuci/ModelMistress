import { describe, expect, it } from 'vitest'
import {
  allowedHint,
  allowedLabel,
  blankForm,
  checkRequest,
  clampRemember,
  formOf,
  modelOptions,
  newBot,
  parseChats,
  SERVICES,
  settingFromForm,
  statusLine,
  tokenNote,
  type BotSetting,
} from './bots'

/** A saved Telegram bot, as config.get shows it: its token is only `(set)`. */
const saved: BotSetting = {
  name: 'news',
  kind: 'telegram',
  token: '(set)',
  model: 'qwen',
  allowed_chats: ['42', '-7'],
  enabled: true,
  api_base: '',
  gateway: '',
  instructions: 'Be brief.',
  remember: 12,
}

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

describe('the bot form', () => {
  it('shows a saved bot without its token, and keeps the token when nothing is typed', () => {
    const f = formOf(saved)
    expect(f.hasSavedToken).toBe(true)
    expect(f.typedToken).toBe('')
    expect(settingFromForm(f).token).toBe('(set)')
    expect(tokenNote(f)).toContain('A token is saved')
  })

  it('uses a typed token in place of the saved one, and sends a cleared token as empty', () => {
    const f = formOf(saved)
    f.typedToken = '  999:NEW  '
    expect(settingFromForm(f).token).toBe('999:NEW')
    f.typedToken = ''
    f.clearToken = true
    expect(settingFromForm(f).token).toBe('')
    expect(tokenNote(f)).toContain('removed when you save')
  })

  it('sends a renamed bot with its previous name, so the token moves with it', () => {
    const f = formOf(saved)
    f.name = 'news-2'
    expect(settingFromForm(f)).toMatchObject({ name: 'news-2', token: '(set)', previous_name: 'news' })
  })

  it('sends a bot not saved yet with no previous name and no token until one is typed', () => {
    const f = blankForm([saved])
    expect(f.savedName).toBe('')
    expect(f.name).toBe('telegram-1')
    const out = settingFromForm(f)
    expect(out).not.toHaveProperty('previous_name')
    expect(out.token).toBe('')
    expect(tokenNote(f)).toContain('No token yet')
  })

  it('keeps the addresses the form does not show, and reads the auto model as the router choice', () => {
    const f = formOf({ ...saved, api_base: 'http://local', gateway: 'ws://local', model: 'auto' })
    expect(f.model).toBe('')
    expect(settingFromForm(f)).toMatchObject({ api_base: 'http://local', gateway: 'ws://local', model: '' })
  })

  it('sends the allowed ids and the instructions trimmed, and the memory as a whole number from 2 to 200', () => {
    const f = formOf({ ...saved, instructions: '  Be brief.  ' })
    f.allowedText = '42, 7\n -9'
    expect(settingFromForm(f)).toMatchObject({ allowed_chats: ['42', '7', '-9'], instructions: 'Be brief.' })
    expect(clampRemember(1)).toBe(2)
    expect(clampRemember(500)).toBe(200)
    expect(clampRemember(12.7)).toBe(12)
    expect(clampRemember(Number.NaN)).toBe(20)
  })

  it('asks the check for the service and the saved bot, and sends only a token that was typed', () => {
    const f = formOf(saved)
    expect(checkRequest(f)).toEqual({ kind: 'telegram', bot: 'news' })
    f.typedToken = ' 123:TYPED '
    expect(checkRequest(f)).toEqual({ kind: 'telegram', bot: 'news', token: '123:TYPED' })
    const fresh = blankForm([])
    fresh.kind = 'discord'
    fresh.typedToken = 'abc'
    expect(checkRequest(fresh)).toEqual({ kind: 'discord', token: 'abc' })
  })

  it('states what the bot is doing, and keeps the last error on that line', () => {
    expect(statusLine(undefined)).toContain('Not saved yet')
    expect(
      statusLine({ name: 'news', kind: 'telegram', enabled: true, running: true, answered: 3, refused: 1, error: null }),
    ).toBe('Running: 3 answered, 1 refused.')
    expect(
      statusLine({
        name: 'news',
        kind: 'telegram',
        enabled: true,
        running: false,
        answered: 0,
        refused: 0,
        error: 'Telegram refused getUpdates: Unauthorized',
      }),
    ).toBe('Starting… Last error: Telegram refused getUpdates: Unauthorized')
    expect(
      statusLine({ name: 'news', kind: 'telegram', enabled: false, running: false, answered: 0, refused: 0, error: null }),
    ).toBe('Off.')
  })

  it('offers the router as the empty choice, and keeps a model the hub no longer has', () => {
    const options = modelOptions(['qwen', 'gemma'], 'gone')
    expect(options[0]).toEqual({ value: '', label: 'Auto: the router chooses' })
    expect(options.map((o) => o.value)).toEqual(['', 'qwen', 'gemma', 'gone'])
    expect(options[3].label).toContain('not on this hub now')
    expect(modelOptions(['qwen'], 'qwen').map((o) => o.value)).toEqual(['', 'qwen'])
  })

  it('gives each service its own hint about who may talk to the bot', () => {
    expect(allowedHint('discord')).toContain('mention it')
    expect(allowedHint('telegram')).toContain('negative')
  })
})
