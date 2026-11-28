import { describe, expect, it } from 'vitest'
import {
  continueConfig,
  envConfig,
  hermesConfig,
  ollamaBase,
  openAiBase,
  openClawConfig,
  openCodeConfig,
  type ConnectTarget,
} from './connect'

const target: ConnectTarget = { baseUrl: 'http://192.0.2.10:8000/', key: 'mm_test_key', model: 'qwen3:8b' }

describe('connect snippets', () => {
  it('takes the hub address with or without a trailing slash, and adds /v1 for OpenAI clients only', () => {
    expect(openAiBase('http://127.0.0.1:8000/')).toBe('http://127.0.0.1:8000/v1')
    expect(openAiBase(' http://127.0.0.1:8000 ')).toBe('http://127.0.0.1:8000/v1')
    expect(ollamaBase('http://127.0.0.1:8000/')).toBe('http://127.0.0.1:8000')
  })

  it('writes an OpenCode provider that parses as JSON and points at /v1 with the key and the model', () => {
    const config = JSON.parse(openCodeConfig(target))
    expect(config.provider.modelmistress.npm).toBe('@ai-sdk/openai-compatible')
    expect(config.provider.modelmistress.options).toEqual({
      baseURL: 'http://192.0.2.10:8000/v1',
      apiKey: 'mm_test_key',
    })
    expect(Object.keys(config.provider.modelmistress.models)).toEqual(['qwen3:8b'])
    expect(config.model).toBe('modelmistress/qwen3:8b')
  })

  it('writes the Continue model with apiBase including /v1, quoted so a model id with a colon stays a string', () => {
    const yaml = continueConfig(target)
    expect(yaml).toContain('    provider: openai\n')
    expect(yaml).toContain('    apiBase: "http://192.0.2.10:8000/v1"\n')
    expect(yaml).toContain('    model: "qwen3:8b"\n')
    expect(yaml).toContain('    apiKey: "mm_test_key"\n')
  })

  it('writes the Hermes custom endpoint and the OpenClaw provider with the same address', () => {
    expect(hermesConfig(target)).toContain('  base_url: "http://192.0.2.10:8000/v1"\n')
    expect(hermesConfig(target)).toContain('  provider: custom\n')
    const oc = JSON.parse(openClawConfig(target))
    expect(oc.models.providers.modelmistress.baseUrl).toBe('http://192.0.2.10:8000/v1')
    expect(oc.models.providers.modelmistress.apiKey).toBe('mm_test_key')
  })

  it('gives the standard OpenAI variables for bash and for PowerShell', () => {
    const env = envConfig(target)
    expect(env.bash).toContain('export OPENAI_BASE_URL="http://192.0.2.10:8000/v1"')
    expect(env.bash).toContain('export OPENAI_API_KEY="mm_test_key"')
    expect(env.powershell).toContain('$env:OPENAI_BASE_URL = "http://192.0.2.10:8000/v1"')
  })
})
