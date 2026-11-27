// Cuts a reply that is still being written into sentences, so each one can be spoken as soon as it is complete. A port of
// the Android SentenceCutter: text inside <think> tags and markdown marks are never spoken.

const THINK_BLOCK = /<think>[\s\S]*?<\/think>/g
const MARKDOWN_MARKS = /[*#`~_|]/g

/** The reply without its thinking: complete think blocks are removed, and an unfinished one hides the rest. */
export function withoutThinking(reply: string): string {
  const closed = reply.replace(THINK_BLOCK, '')
  const open = closed.indexOf('<think>')
  return open >= 0 ? closed.slice(0, open) : closed
}

/** The text as it is said: markdown marks dropped, spaces collapsed. Null when nothing is left to say. */
export function cleanSentence(piece: string): string | null {
  const said = piece.replace(MARKDOWN_MARKS, '').replace(/\s+/g, ' ').trim()
  return /[\p{L}\p{N}]/u.test(said) ? said : null
}

export class SentenceCutter {
  /** How much of the visible reply has been cut so far. */
  private spokenUpTo = 0

  /**
   * Call with the reply so far each time it grows. Each sentence comes back once. Call it once more with `final` set when
   * the reply is finished, which also returns the unfinished last piece.
   */
  cut(reply: string, final = false): string[] {
    const visible = withoutThinking(reply)
    // A reply that shrank (a tool step cleared the text) starts again from its beginning.
    if (visible.length < this.spokenUpTo) this.spokenUpTo = 0
    const out: string[] = []
    for (let index = this.spokenUpTo; index < visible.length; index++) {
      const c = visible[index]
      let ends = false
      if (c === '\n') ends = true
      else if (c === '.' || c === '!' || c === '?') {
        const next = visible[index + 1]
        ends = next === undefined ? final : /\s/.test(next)
      }
      if (ends) {
        const said = cleanSentence(visible.slice(this.spokenUpTo, index + 1))
        if (said) out.push(said)
        this.spokenUpTo = index + 1
      }
    }
    if (final && this.spokenUpTo < visible.length) {
      const said = cleanSentence(visible.slice(this.spokenUpTo))
      if (said) out.push(said)
      this.spokenUpTo = visible.length
    }
    return out
  }
}
