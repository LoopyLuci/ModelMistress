import { describe, expect, it } from 'vitest'
import { conversationAssets, extOf, extractAssets, isHubPath, kindOf, safeSrc } from './media'

describe('media in a chat', () => {
  it('knows what a URL is from its extension or data type', () => {
    expect(extOf('https://x.test/a/b.PNG?v=2#f')).toBe('png')
    expect(kindOf('https://x.test/clip.mp4')).toBe('video')
    expect(kindOf('/v1/images/files/x.png')).toBe('image')
    expect(kindOf('data:audio/wav;base64,AAAA')).toBe('audio')
    expect(kindOf('data:image/svg+xml;base64,AAAA')).toBe('svg')
    expect(kindOf('data:model/gltf-binary;base64,AAAA')).toBe('model')
    expect(kindOf('https://x.test/page')).toBeNull()
  })

  it('never takes a script address as an asset', () => {
    expect(safeSrc('javascript:alert(1)')).toBe(false)
    expect(safeSrc('//evil.test/a.png')).toBe(false)
    expect(safeSrc('/v1/images/files/a.png')).toBe(true)
    expect(extractAssets('[x](javascript:alert(1).png)')).toEqual([])
  })

  it('finds markdown pictures, links, bare URLs and fenced drawings, once each', () => {
    const t = [
      '![a fox](/v1/images/files/fox.png)',
      'See [the clip](https://x.test/c.webm) and https://x.test/song.mp3.',
      'Again https://x.test/c.webm',
      '[model](https://x.test/chair.glb)',
      '```svg',
      '<svg xmlns="http://www.w3.org/2000/svg"><circle r="5"/></svg>',
      '```',
      '```js',
      'const u = "https://x.test/hidden.png"',
      '```',
    ].join('\n')
    const a = extractAssets(t, '3')
    expect(a.map((x) => x.kind)).toEqual(['svg', 'image', 'video', 'model', 'audio'])
    expect(a.find((x) => x.kind === 'model')?.format).toBe('glb')
    expect(a.find((x) => x.kind === 'image')?.title).toBe('a fox')
    expect(a.every((x) => x.id.startsWith('3-'))).toBe(true)
    expect(a.some((x) => x.src.includes('hidden'))).toBe(false)
  })

  it('collects a conversation, pictures attached by the user included', () => {
    const a = conversationAssets([{ content: 'hi', images: ['data:image/png;base64,AAAA'] }, { content: '![z](https://x.test/z.png)' }])
    expect(a.map((x) => x.id)).toEqual(['0-p0', '1-0'])
    expect(isHubPath('/v1/x.png')).toBe(true)
    expect(isHubPath('//x')).toBe(false)
  })
})
