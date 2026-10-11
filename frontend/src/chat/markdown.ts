// A small Markdown renderer for chat replies. Everything is escaped first and only the tags built here come out, so a
// model (or a pasted text) cannot inject HTML or script; links are limited to http, https and mailto.

const esc = (s: string): string =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;')

const safeUrl = (u: string): string | null => {
  const t = u.trim().replace(/&amp;/g, '&')
  return /^(https?:\/\/|mailto:)/i.test(t) ? esc(t) : null
}

/** Inline spans of one already-unescaped line of text. */
function inline(src: string): string {
  // code spans first (their content is literal), the rest in between
  const parts = src.split(/(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)/)
  let out = ''
  for (let i = 0; i < parts.length; i += 3) {
    out += spans(esc(parts[i] ?? ''))
    if (i + 2 < parts.length) out += `<code>${esc(parts[i + 2].trim())}</code>`
  }
  return out
}

function spans(s: string): string {
  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)(?:\s+&quot;[^)]*&quot;)?\)/g, (m, text, url) => {
    const u = safeUrl(url)
    return u ? `<a href="${u}" target="_blank" rel="noopener noreferrer">${text}</a>` : m
  })
  s = s.replace(/\*\*([^*\n]+?)\*\*|__([^_\n]+?)__/g, (_m, a, b) => `<strong>${a ?? b}</strong>`)
  s = s.replace(/(^|[^*\w])\*([^*\s][^*\n]*?)\*(?!\*)/g, '$1<em>$2</em>')
  s = s.replace(/(^|[^_\w])_([^_\s][^_\n]*?)_(?![_\w])/g, '$1<em>$2</em>')
  s = s.replace(/~~([^~\n]+?)~~/g, '<del>$1</del>')
  return s
}

const cells = (line: string): string[] =>
  line
    .trim()
    .replace(/^\|/, '')
    .replace(/\|$/, '')
    .split('|')
    .map((c) => c.trim())

/** Markdown to safe HTML. An unclosed code fence (a reply still streaming) renders as an open block. */
export function renderMarkdown(text: string): string {
  const lines = text.replace(/\r\n?/g, '\n').split('\n')
  const out: string[] = []
  let i = 0
  const isBlockStart = (l: string) =>
    /^\s*(```|#{1,6}\s|>|[-*+]\s|\d+[.)]\s)/.test(l) || /^\s*([-*_])(\s*\1){2,}\s*$/.test(l)
  while (i < lines.length) {
    const line = lines[i]
    const fence = line.match(/^\s*(`{3,}|~{3,})\s*([\w+#.-]*)/)
    if (fence) {
      const body: string[] = []
      i++
      while (i < lines.length && !lines[i].trim().startsWith(fence[1])) body.push(lines[i++])
      i++ // the closing fence
      const lang = esc(fence[2] || '')
      out.push(
        `<div class="codeblock"><div class="cbhead"><span>${lang || 'text'}</span><button type="button" class="cbcopy" data-copy>Copy</button></div>` +
          `<pre><code>${esc(body.join('\n'))}</code></pre></div>`,
      )
      continue
    }
    if (!line.trim()) {
      i++
      continue
    }
    const h = line.match(/^\s*(#{1,6})\s+(.*?)\s*#*\s*$/)
    if (h) {
      out.push(`<h${h[1].length + 2}>${inline(h[2])}</h${h[1].length + 2}>`)
      i++
      continue
    }
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) {
      out.push('<hr>')
      i++
      continue
    }
    if (/^\s*>/.test(line)) {
      const q: string[] = []
      while (i < lines.length && /^\s*>/.test(lines[i])) q.push(lines[i++].replace(/^\s*>\s?/, ''))
      out.push(`<blockquote>${renderMarkdown(q.join('\n'))}</blockquote>`)
      continue
    }
    const li = line.match(/^(\s*)([-*+]|\d+[.)])\s+/)
    if (li) {
      const ordered = /\d/.test(li[2])
      const items: string[] = []
      while (i < lines.length) {
        const m = lines[i].match(/^(\s*)([-*+]|\d+[.)])\s+(.*)$/)
        if (m && /\d/.test(m[2]) === ordered) {
          items.push(m[3])
          i++
        } else if (/^\s{2,}\S/.test(lines[i]) && items.length) {
          items[items.length - 1] += ' ' + lines[i++].trim() // a wrapped item
        } else break
      }
      const tag = ordered ? 'ol' : 'ul'
      out.push(`<${tag}>${items.map((t) => `<li>${inline(t)}</li>`).join('')}</${tag}>`)
      continue
    }
    if (line.includes('|') && i + 1 < lines.length && /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(lines[i + 1])) {
      const head = cells(line)
      i += 2
      const rows: string[][] = []
      while (i < lines.length && lines[i].includes('|') && lines[i].trim()) rows.push(cells(lines[i++]))
      out.push(
        `<div class="tablewrap"><table><thead><tr>${head.map((c) => `<th>${inline(c)}</th>`).join('')}</tr></thead><tbody>` +
          rows.map((r) => `<tr>${r.map((c) => `<td>${inline(c)}</td>`).join('')}</tr>`).join('') +
          '</tbody></table></div>',
      )
      continue
    }
    const para: string[] = []
    while (i < lines.length && lines[i].trim() && !(para.length && isBlockStart(lines[i]))) para.push(lines[i++])
    out.push(`<p>${para.map(inline).join('<br>')}</p>`)
  }
  return out.join('')
}

/** Splits a reply into its reasoning and its answer: `<think>...</think>` in the text (an unclosed one is still
 *  being thought). */
export function splitThink(content: string): { reasoning: string; answer: string; thinking: boolean } {
  let reasoning = ''
  let answer = ''
  let rest = content
  let thinking = false
  for (;;) {
    const a = rest.indexOf('<think>')
    if (a < 0) {
      // a stray close without an open: everything before it was reasoning (the template opened the tag)
      const c = rest.indexOf('</think>')
      if (c >= 0 && !reasoning) {
        reasoning += rest.slice(0, c)
        rest = rest.slice(c + 8)
        continue
      }
      answer += rest
      break
    }
    answer += rest.slice(0, a)
    const b = rest.indexOf('</think>', a + 7)
    if (b < 0) {
      reasoning += rest.slice(a + 7)
      thinking = true
      break
    }
    reasoning += rest.slice(a + 7, b)
    rest = rest.slice(b + 8)
  }
  return { reasoning: reasoning.trim(), answer: answer.replace(/^\s+/, ''), thinking }
}
