/** One bot as the settings hold it (config.get). The token is `(set)` when one is saved; sending that back keeps it. */
export interface BotSetting {
  name: string
  kind: string
  token: string
  model: string
  allowed_chats: string[]
  enabled: boolean
  api_base: string
  gateway: string
  /** What the model is told at the start of each conversation. Empty: nothing. */
  instructions: string
  /** Messages of a conversation the bot keeps for the model; 0 means the default. */
  remember: number
}

/** A bot's live state (bots.status): what it is doing now, and how it has answered. */
export interface BotStatus {
  name: string
  kind: string
  enabled: boolean
  running: boolean
  answered: number
  refused: number
  error: string | null
}

export const DEFAULT_REMEMBER = 20
export const MAX_REMEMBER = 200
export const MAX_INSTRUCTIONS = 4000

/** Chat ids as typed: numbers separated by commas, spaces or new lines. */
export function parseChats(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
}

/** A new Telegram bot with a name nobody has, off, with no chat allowed. */
export function newBot(existing: Pick<BotSetting, 'name'>[]): BotSetting {
  let n = 1
  while (existing.some((b) => b.name === `telegram-${n}`)) n++
  return {
    name: `telegram-${n}`,
    kind: 'telegram',
    token: '',
    model: '',
    allowed_chats: [],
    enabled: false,
    api_base: '',
    gateway: '',
    instructions: '',
    remember: DEFAULT_REMEMBER,
  }
}

/** The services a bot can talk through, as the settings name them. */
export const SERVICES = [
  { id: 'telegram', label: 'Telegram' },
  { id: 'discord', label: 'Discord' },
]

export function serviceLabel(kind: string): string {
  return SERVICES.find((s) => s.id === kind)?.label ?? kind
}

/** The label of the list of ids allowed to use a bot: Telegram chats, or Discord users. */
export function allowedLabel(kind: string): string {
  return kind === 'discord' ? 'People allowed (Discord user ids)' : 'Chats allowed (Telegram chat ids)'
}

/** A bot as the form edits it. Fields the form does not show (the addresses) go back unchanged. */
export interface BotForm {
  /** The name the bot was saved under; empty for a bot not saved yet. */
  savedName: string
  name: string
  kind: string
  enabled: boolean
  /** The model's id; empty means the router chooses (`auto`). */
  model: string
  allowedText: string
  instructions: string
  remember: number
  /** A token is saved for this bot. The form never shows it. */
  hasSavedToken: boolean
  /** A token typed in the form; empty keeps the saved one. */
  typedToken: string
  /** The saved token is removed on save. */
  clearToken: boolean
  api_base: string
  gateway: string
}

/** The form for a saved bot. A model of `auto` is the router's choice, which the form shows as empty. */
export function formOf(b: BotSetting): BotForm {
  return {
    savedName: b.name,
    name: b.name,
    kind: b.kind,
    enabled: b.enabled,
    model: b.model === 'auto' ? '' : b.model || '',
    allowedText: (b.allowed_chats || []).join(', '),
    instructions: b.instructions || '',
    remember: b.remember || DEFAULT_REMEMBER,
    hasSavedToken: b.token === '(set)',
    typedToken: '',
    clearToken: false,
    api_base: b.api_base || '',
    gateway: b.gateway || '',
  }
}

/** The form for a new bot: a Telegram bot, off, with no token and no chat. */
export function blankForm(existing: Pick<BotSetting, 'name'>[]): BotForm {
  return { ...formOf(newBot(existing)), savedName: '' }
}

/** Remembered messages as the settings take them: a whole number from 2 to 200. */
export function clampRemember(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_REMEMBER
  return Math.min(MAX_REMEMBER, Math.max(2, Math.floor(n)))
}

/**
 * The bot as config.set takes it. The token is the typed one, else the saved one (sent as `(set)`), else none; a cleared
 * token is sent empty. A saved bot carries its previous name, so a rename keeps its token.
 */
export function settingFromForm(f: BotForm): BotSetting & { previous_name?: string } {
  const typed = f.typedToken.trim()
  const token = f.clearToken ? '' : typed || (f.hasSavedToken ? '(set)' : '')
  const bot: BotSetting & { previous_name?: string } = {
    name: f.name.trim(),
    kind: f.kind,
    token,
    model: f.model.trim(),
    allowed_chats: parseChats(f.allowedText),
    enabled: f.enabled,
    api_base: f.api_base,
    gateway: f.gateway,
    instructions: f.instructions.trim(),
    remember: clampRemember(f.remember),
  }
  if (f.savedName) bot.previous_name = f.savedName
  return bot
}

/** What bots.check needs: the service, the bot (for its saved token) and any token typed. */
export function checkRequest(f: BotForm): { kind: string; bot?: string; token?: string } {
  const req: { kind: string; bot?: string; token?: string } = { kind: f.kind }
  if (f.savedName) req.bot = f.savedName
  const typed = f.typedToken.trim()
  if (typed) req.token = typed
  return req
}

/** One line under the token field: what will happen to the token on save. */
export function tokenNote(f: BotForm): string {
  if (f.clearToken) return 'The saved token is removed when you save bots.'
  if (f.typedToken.trim()) return 'A new token. Check it, then save bots to keep it.'
  if (f.hasSavedToken) return 'A token is saved. Leave this empty to keep it, or type a new one to replace it.'
  const where = f.kind === 'discord' ? 'the Discord developer portal (the Bot page)' : '@BotFather'
  return `No token yet. Paste the token from ${where}.`
}

/** What the bot is doing, from its live status; the last error is kept on the line. */
export function statusLine(s: BotStatus | undefined): string {
  if (!s) return 'Not saved yet. Save bots to start it.'
  const base = s.running ? `Running: ${s.answered} answered, ${s.refused} refused.` : s.enabled ? 'Starting…' : 'Off.'
  return s.error ? `${base} Last error: ${s.error}` : base
}

/**
 * The model choices: the router first, then the hub's chat models. A model the hub no longer has is kept as a choice, so
 * a saved setting is not silently lost.
 */
export function modelOptions(ids: string[], current: string): { value: string; label: string }[] {
  const options = [{ value: '', label: 'Auto: the router chooses' }]
  for (const id of ids) options.push({ value: id, label: id })
  if (current && !ids.includes(current)) options.push({ value: current, label: `${current} (not on this hub now)` })
  return options
}

/** The hint under the list of ids allowed to use a bot, for its service. */
export function allowedHint(kind: string): string {
  return kind === 'discord'
    ? 'A Discord user id. In a server the bot answers only the messages that mention it.'
    : 'A chat id. A group has a negative id. Someone not on the list is told their id, to add here.'
}
