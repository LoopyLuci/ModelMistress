// Copies a model file the owner picks (a .gguf) into the hub's models folder, where the model list finds it.
import { failure, hubFetch } from '@/chat/hub'

export interface Imported {
  name: string
  bytes: number
  path: string
}

/** The file goes up as its body. Its name travels percent-encoded, because a header is ASCII only. */
export async function importModel(f: File): Promise<Imported> {
  const r = await hubFetch('/v1/models/import', {
    method: 'POST',
    headers: { 'Content-Type': 'application/octet-stream', 'x-file-name': encodeURIComponent(f.name) },
    body: f,
  })
  if (!r.ok) throw await failure(r)
  return r.json()
}
