import { describe, expect, it } from 'vitest'
import { renderMarkdown, splitThink } from './markdown'

describe('renderMarkdown', () => {
  it('escapes HTML a model or pasted text sends, so nothing becomes a tag', () => {
    const html = renderMarkdown('<script>alert(1)</script> & <img src=x onerror=alert(1)>')
    expect(html).not.toContain('<script>')
    expect(html).not.toContain('<img')
    expect(html).toContain('&lt;script&gt;')
    expect(html).toContain('&amp;')
  })

  it('renders only http, https and mailto links', () => {
    expect(renderMarkdown('[site](https://example.com)')).toContain(
      '<a href="https://example.com" target="_blank" rel="noopener noreferrer">site</a>',
    )
    expect(renderMarkdown('[bad](javascript:alert(1))')).not.toContain('<a ')
  })

  it('renders emphasis, inline code and headings', () => {
    expect(renderMarkdown('**bold** and `a < b`')).toContain('<strong>bold</strong>')
    expect(renderMarkdown('`a < b`')).toContain('<code>a &lt; b</code>')
    expect(renderMarkdown('## Title')).toContain('<h4>Title</h4>')
  })

  it('renders a fenced code block with its language and escaped content', () => {
    const html = renderMarkdown('```js\nconst a = 1 < 2\n```')
    expect(html).toContain('<span>js</span>')
    expect(html).toContain('<pre><code>const a = 1 &lt; 2</code></pre>')
  })

  it('keeps an unclosed fence open, as a reply still streaming', () => {
    expect(renderMarkdown('```py\nprint(1)')).toContain('<pre><code>print(1)</code></pre>')
  })

  it('renders lists and tables', () => {
    expect(renderMarkdown('- a\n- b')).toContain('<ul><li>a</li><li>b</li></ul>')
    const table = renderMarkdown('| a | b |\n|---|---|\n| 1 | 2 |')
    expect(table).toContain('<th>a</th>')
    expect(table).toContain('<td>1</td>')
  })
})

describe('splitThink', () => {
  it('separates a closed reasoning block from the answer', () => {
    expect(splitThink('<think>plan</think>Answer')).toEqual({ reasoning: 'plan', answer: 'Answer', thinking: false })
  })

  it('reports an unclosed reasoning block as still thinking', () => {
    expect(splitThink('<think>still')).toEqual({ reasoning: 'still', answer: '', thinking: true })
  })

  it('treats text before a stray close tag as reasoning', () => {
    expect(splitThink('reasoning</think>answer')).toEqual({ reasoning: 'reasoning', answer: 'answer', thinking: false })
  })
})
