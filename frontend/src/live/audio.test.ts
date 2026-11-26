import { afterEach, describe, expect, it, vi } from 'vitest'
import { LiveMic } from './audio'

// A stand-in microphone and audio context: the test sees the constraints asked for, and what the browser reports applied.
function stubMicrophone(settings: MediaTrackSettings) {
  const requests: MediaStreamConstraints[] = []
  const stream = {
    getAudioTracks: () => [{ getSettings: () => settings }],
    getTracks: () => [],
  }
  Object.defineProperty(navigator, 'mediaDevices', {
    configurable: true,
    value: {
      getUserMedia: vi.fn(async (constraints: MediaStreamConstraints) => {
        requests.push(constraints)
        return stream
      }),
    },
  })
  class FakeContext {
    createAnalyser() {
      return { fftSize: 0, getFloatTimeDomainData: () => {} }
    }
    createMediaStreamSource() {
      return { connect: () => {} }
    }
    close() {
      return Promise.resolve()
    }
  }
  vi.stubGlobal('AudioContext', FakeContext)
  return requests
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('LiveMic.open', () => {
  it('asks for echo cancellation, noise suppression and automatic gain, and reads back what the browser applied', async () => {
    const requests = stubMicrophone({ echoCancellation: false })
    const mic = new LiveMic({ onUtterance: () => {} })
    await mic.open()
    expect(requests[0].audio).toEqual({ echoCancellation: true, noiseSuppression: true, autoGainControl: true })
    expect(mic.echoCancelling).toBe(false)
    mic.close()
  })

  it('reports echo cancellation as on when the browser applies it', async () => {
    stubMicrophone({ echoCancellation: true })
    const mic = new LiveMic({ onUtterance: () => {} })
    await mic.open()
    expect(mic.echoCancelling).toBe(true)
    mic.close()
  })

  it('reports null when the browser does not say whether echo cancellation is applied', async () => {
    stubMicrophone({})
    const mic = new LiveMic({ onUtterance: () => {} })
    expect(mic.echoCancelling).toBeNull()
    await mic.open()
    expect(mic.echoCancelling).toBeNull()
    mic.close()
  })
})
