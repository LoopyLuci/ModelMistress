// The camera for a chat picture: the browser's camera (getUserMedia) shown in a video element, and one frame taken
// from it as a JPEG data URL, the same size limit as a picture from the disk.

export async function startCamera(): Promise<MediaStream> {
  if (!navigator.mediaDevices?.getUserMedia) throw new Error('This window has no camera access.')
  return navigator.mediaDevices.getUserMedia({ video: { width: { ideal: 1280 }, height: { ideal: 720 } }, audio: false })
}

export function stopCamera(stream: MediaStream | null | undefined) {
  stream?.getTracks().forEach((t) => t.stop())
}

/** One frame of the video as a JPEG data URL, scaled so its longest side is at most 1024 pixels. */
export function captureFrame(video: HTMLVideoElement): string {
  const w = video.videoWidth || 640
  const h = video.videoHeight || 480
  const s = Math.min(1, 1024 / Math.max(w, h))
  const c = document.createElement('canvas')
  c.width = Math.max(1, Math.round(w * s))
  c.height = Math.max(1, Math.round(h * s))
  const ctx = c.getContext('2d')
  if (!ctx) throw new Error('The picture could not be drawn.')
  ctx.drawImage(video, 0, 0, c.width, c.height)
  return c.toDataURL('image/jpeg', 0.85)
}
