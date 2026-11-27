// Reads a file the owner attaches to a chat message: text-like files in the browser, PDFs through the hub (which can
// read their text; the browser cannot without a large PDF library).
import { failure, hubFetch } from './hub'
import { attachmentBlock, isPdf, isTextFile } from './extras'

export interface Attachment {
  name: string
  text: string
  cut: boolean
}

export async function readAttachment(f: File): Promise<Attachment> {
  let text: string
  if (isPdf(f)) {
    const r = await hubFetch('/v1/documents/text', {
      method: 'POST',
      headers: { 'Content-Type': 'application/pdf' },
      body: f,
    })
    if (!r.ok) throw await failure(r)
    text = String((await r.json()).text || '')
    if (!text.trim()) throw new Error(`${f.name} has no text in it (a scanned PDF needs OCR, which this build does not have).`)
  } else if (isTextFile(f)) {
    text = await f.text()
  } else {
    throw new Error(`${f.name}: only text, source, JSON, and PDF files can be attached.`)
  }
  const block = attachmentBlock(f.name, text)
  return { name: f.name, text: block.text, cut: block.cut }
}
