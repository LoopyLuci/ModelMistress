/** A ModelMistress on another device, as the settings hold it (config.get). The key is `(set)` when one is saved; sending that back keeps it. */
export interface DeviceSetting {
  name: string
  url: string
  key: string
}

/** What `device.check` reports: whether the device's hub answered, and what it said. */
export interface DeviceCheck {
  device: string
  reachable: boolean
  status: number | null
  answer: string
}

/** A new device with a name nobody has, and no address yet. */
export function newDevice(existing: DeviceSetting[]): DeviceSetting {
  let n = 1
  while (existing.some((d) => d.name === `phone-${n}`)) n++
  return { name: `phone-${n}`, url: '', key: '' }
}

/** Only a model file the hub holds goes to a device. An Ollama model is stored as blobs, so it is not offered. */
export function canSendTo(model: { source: string }): boolean {
  return !model.source.startsWith('ollama')
}
