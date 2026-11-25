// Media in a chat: what a message contains that is not text (pictures, video, audio, 3D models, vector drawings, pages,
// documents, data), found in its markdown, so the canvas can show it. Any model's reply works: nothing here depends on
// who wrote it.

export type AssetKind = 'image' | 'video' | 'audio' | 'model' | 'svg' | 'html' | 'pdf' | 'text' | 'file'

export interface Asset {
  /** Stable within a message: `<message index>-<n>`. */
  id: string
  kind: AssetKind
  /** A URL, a data: URL, or a path on the hub (`/v1/images/files/...`, which needs the owner token). */
  src: string
  title: string
  /** Inline source for kinds that have no URL (a fenced svg or html block). */
  inline?: string
  /** The 3D format when kind is `model`. */
  format?: 'glb' | 'gltf' | 'obj' | 'stl'
}

const EXT: Record<string, AssetKind> = {
  png: 'image', jpg: 'image', jpeg: 'image', gif: 'image', webp: 'image', avif: 'image', bmp: 'image',
  mp4: 'video', webm: 'video', mov: 'video', m4v: 'video', ogv: 'video',
  mp3: 'audio', wav: 'audio', ogg: 'audio', oga: 'audio', flac: 'audio', m4a: 'audio', opus: 'audio', aac: 'audio',
  glb: 'model', gltf: 'model', obj: 'model', stl: 'model',
  svg: 'svg', html: 'html', htm: 'html', pdf: 'pdf',
  txt: 'text', md: 'text', csv: 'text', tsv: 'text', json: 'text', log: 'text', xml: 'text', yaml: 'text', yml: 'text',
  zip: 'file', ply: 'file', fbx: 'file', usdz: 'file',
}

/** The extension of a URL or file name, without a query or fragment; '' when it has none. */
export function extOf(url: string): string {
  const clean = url.split(/[?#]/)[0]
  const m = /\.([A-Za-z0-9]{2,5})$/.exec(clean)
  return m ? m[1].toLowerCase() : ''
}

/** What a URL points at, from a data: URL's type or the extension. null: not a media URL. */
export function kindOf(url: string): AssetKind | null {
  const d = /^data:([^;,]+)/i.exec(url)
  if (d) {
    const t = d[1].toLowerCase()
    if (t === 'image/svg+xml') return 'svg'
    if (t.startsWith('image/')) return 'image'
    if (t.startsWith('video/')) return 'video'
    if (t.startsWith('audio/')) return 'audio'
    if (t === 'model/gltf-binary' || t === 'model/gltf+json') return 'model'
    if (t === 'application/pdf') return 'pdf'
    if (t === 'text/html') return 'html'
    return t.startsWith('text/') ? 'text' : null
  }
  return EXT[extOf(url)] ?? null
}

export function formatOf(url: string): Asset['format'] | undefined {
  const d = /^data:model\/gltf-binary/i.test(url) ? 'glb' : /^data:model\/gltf\+json/i.test(url) ? 'gltf' : extOf(url)
  return d === 'glb' || d === 'gltf' || d === 'obj' || d === 'stl' ? d : undefined
}

function titleOf(url: string, alt = ''): string {
  if (alt.trim()) return alt.trim()
  if (url.startsWith('data:')) return 'embedded ' + (kindOf(url) || 'file')
  const name = decodeURIComponent(url.split(/[?#]/)[0].split('/').pop() || url)
  return name || url
}

/** Only addresses the page may load: http(s), data:, and hub paths. javascript: and the like are never an asset. */
export function safeSrc(url: string): boolean {
  return /^(https?:\/\/|data:|\/(?!\/))/i.test(url.trim())
}

/** The assets in a message's text: markdown images and links, bare URLs, and fenced svg / html blocks. In order, no repeats. */
export function extractAssets(text: string, prefix = '0'): Asset[] {
  const out: Asset[] = []
  const seen = new Set<string>()
  const add = (a: Omit<Asset, 'id'>, key = a.src) => {
    if (seen.has(key)) return
    seen.add(key)
    out.push({ ...a, id: `${prefix}-${out.length}` })
  }
  // fenced blocks that are a document of their own
  const body = (text || '').replace(/```(svg|html)\s*\n([\s\S]*?)```/gi, (_m, lang: string, code: string) => {
    const kind = lang.toLowerCase() as 'svg' | 'html'
    add({ kind, src: '', inline: code.trim(), title: kind === 'svg' ? 'drawing' : 'page' }, `inline:${kind}:${code.length}:${code.slice(0, 40)}`)
    return ''
  })
  const unfenced = body.replace(/```[\s\S]*?```/g, '')
  const link = /!?\[([^\]]*)\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)/g
  let m: RegExpExecArray | null
  while ((m = link.exec(unfenced))) {
    const url = m[2]
    const kind = kindOf(url)
    if (kind && safeSrc(url)) add({ kind, src: url, title: titleOf(url, m[1]), format: formatOf(url) })
  }
  const bare = /(?<![(\]"'<])(https?:\/\/[^\s<>)"']+)/g
  while ((m = bare.exec(unfenced))) {
    const url = m[1].replace(/[.,;:!?]+$/, '')
    const kind = kindOf(url)
    if (kind) add({ kind, src: url, title: titleOf(url), format: formatOf(url) })
  }
  return out
}

/** The assets of a whole conversation, newest message last, ids unique across messages. */
export function conversationAssets(messages: { content?: string; images?: string[] }[]): Asset[] {
  const all: Asset[] = []
  messages.forEach((m, i) => {
    ;(m.images || []).forEach((u, j) => {
      if (safeSrc(u)) all.push({ id: `${i}-p${j}`, kind: kindOf(u) || 'image', src: u, title: 'attached picture' })
    })
    all.push(...extractAssets(m.content || '', String(i)))
  })
  return all
}

/** True for a path on the hub: loading it needs the owner token, so it is fetched and shown from a blob. */
export const isHubPath = (src: string): boolean => src.startsWith('/') && !src.startsWith('//')
