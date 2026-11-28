/** One bot as the settings hold it (config.get). The token is `(set)` when one is saved; sending that back keeps it. */
export interface BotSetting {
  name: string
  kind: string
  token: string
  model: string
  allowed_chats: string[]
  enabled: boolean
  api_base: string
}

/** Chat ids as typed: numbers separated by commas, spaces or new lines. */
export function parseChats(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
}

/** A new Telegram bot with a name nobody has, off, with no chat allowed. */
export function newBot(existing: BotSetting[]): BotSetting {
  let n = 1
  while (existing.some((b) => b.name === `telegram-${n}`)) n++
  return { name: `telegram-${n}`, kind: 'telegram', token: '', model: '', allowed_chats: [], enabled: false, api_base: '' }
}

/** The services a bot can talk through, as the settings name them. */
export const SERVICES = [
  { id: 'telegram', label: 'Telegram' },
  { id: 'discord', label: 'Discord' },
]

/** The label of the list of ids allowed to use a bot: Telegram chats, or Discord users. */
export function allowedLabel(kind: string): string {
  return kind === 'discord' ? 'People allowed (Discord user ids)' : 'Chats allowed (Telegram chat ids)'
}
