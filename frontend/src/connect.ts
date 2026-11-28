/**
 * Config snippets for other software that uses ModelMistress as its model provider, the way it uses Ollama or LM Studio.
 * Each snippet takes the hub's address (no path, e.g. http://127.0.0.1:8000), a peer key made for that software (never
 * the owner's token), and one model id. The formats are the ones each tool's own documentation shows; where a field was
 * not confirmed, the snippet says so in its note.
 */

export interface ConnectTarget {
  /** The hub's address, with no path: http://host:port */
  baseUrl: string
  /** A peer key from key.create. Shown once when it was made; the snippets carry it as given. */
  key: string
  /** A model id the hub lists (GET /v1/models). */
  model: string
}

/** The OpenAI-compatible API root: the address with /v1. */
export function openAiBase(baseUrl: string): string {
  return `${trimSlash(baseUrl)}/v1`
}

/** The Ollama-compatible API root: the address itself (ModelMistress serves it under /api). */
export function ollamaBase(baseUrl: string): string {
  return trimSlash(baseUrl)
}

function trimSlash(s: string): string {
  return s.trim().replace(/\/+$/, '')
}

/** A YAML double-quoted scalar. JSON string syntax is valid YAML, so this is enough for ids, URLs and keys. */
function yamlString(s: string): string {
  return JSON.stringify(s)
}

/** OpenCode CLI: a custom provider in opencode.json (project folder, or ~/.config/opencode/opencode.json for all). */
export function openCodeConfig(t: ConnectTarget): string {
  const config = {
    provider: {
      modelmistress: {
        npm: '@ai-sdk/openai-compatible',
        name: 'ModelMistress',
        options: { baseURL: openAiBase(t.baseUrl), apiKey: t.key },
        models: { [t.model]: { name: t.model } },
      },
    },
    model: `modelmistress/${t.model}`,
  }
  return JSON.stringify(config, null, 2)
}

/** Continue (the VS Code and JetBrains extension): a model in ~/.continue/config.yaml. */
export function continueConfig(t: ConnectTarget): string {
  return [
    'name: ModelMistress',
    'version: 1.0.0',
    'schema: v1',
    'models:',
    `  - name: ${yamlString(t.model)}`,
    '    provider: openai',
    `    model: ${yamlString(t.model)}`,
    `    apiBase: ${yamlString(openAiBase(t.baseUrl))}`,
    `    apiKey: ${yamlString(t.key)}`,
    '',
  ].join('\n')
}

/**
 * Hermes Agent: a custom endpoint in ~/.hermes/config.yaml. `hermes model` writes the same settings when the custom
 * endpoint is picked there, so that is the first thing to try. The key is also read from OPENAI_API_KEY.
 */
export function hermesConfig(t: ConnectTarget): string {
  return [
    'model:',
    '  provider: custom',
    `  base_url: ${yamlString(openAiBase(t.baseUrl))}`,
    `  default: ${yamlString(t.model)}`,
    '',
  ].join('\n')
}

/** OpenClaw: a provider under models.providers in ~/.openclaw/openclaw.json. */
export function openClawConfig(t: ConnectTarget): string {
  const config = {
    models: {
      providers: {
        modelmistress: {
          baseUrl: openAiBase(t.baseUrl),
          apiKey: t.key,
        },
      },
    },
  }
  return JSON.stringify(config, null, 2)
}

/** Any OpenAI SDK or tool that reads the standard variables (bash, then PowerShell). */
export function envConfig(t: ConnectTarget): { bash: string; powershell: string } {
  const base = openAiBase(t.baseUrl)
  return {
    bash: [`export OPENAI_BASE_URL="${base}"`, `export OPENAI_API_KEY="${t.key}"`].join('\n'),
    powershell: [`$env:OPENAI_BASE_URL = "${base}"`, `$env:OPENAI_API_KEY = "${t.key}"`].join('\n'),
  }
}
