# Text to speech

ModelMistress speaks text with llama.cpp's own `llama-tts` tool, built from the vendored llama.cpp (`engines/llama.cpp`)
by the same build that makes `llama-server`. No Python, no separate server.

## Building

Nothing extra: `llama-tts` is built and embedded with the built-in llama.cpp.

```bash
cargo build --release -p model-mistress --features bundled-engine
```

Like `llama-server`, it is written to `<home>/engines/bundled/<hash>/` on first start. Without `bundled-engine`,
ModelMistress looks for `llama-tts` beside the `llama-server` it uses (the `llama_server` setting, `<home>/engines/llama.cpp`,
or `PATH`); if it is missing the endpoint answers `501` and says so.

## Models

llama.cpp's TTS tool is model-agnostic. A speech model is two GGUF files, and the tool needs both:

- the **model** (the "backbone"), and
- its **projector**, `mmproj*.gguf`, which holds the audio decoder.

ModelMistress pairs them the way it pairs a vision model with its projector: the `mmproj*.gguf` in the same folder as
the model file is used. Models that llama.cpp supports for speech at the pinned commit are listed in
`engines/llama.cpp/tools/tts/README.md` (Qwen3-TTS and Pocket TTS at the time of writing). For example:

```bash
model-mistress pull hf.co/ggml-org/Qwen3-TTS-12Hz-1.7B-Base-GGUF
```

downloads the model and its projector together. (The older OuteTTS + WavTokenizer pairing is not what the pinned
`llama-tts` runs any more.)

The model used for a request:

1. the request's `model`, a model id from `model.list`;
2. for no model, `tts-1`, `tts-1-hd` or `gpt-4o-mini-tts` (what OpenAI clients send): the `tts_model` setting, else
   the only model that has a projector and "tts" in its id or architecture;
3. with none found: `404` and a message saying what to download.

A named model without a projector gets a `400`.

## Voices

A voice is a short reference recording (WAV, MP3 or FLAC) that the model imitates. Some models need one (Pocket TTS
makes almost no sound without it), others speak in a default voice.

- `voice` is the name of a recording in `<home>/voices` without the extension (`anna` is `<home>/voices/anna.wav`,
  `.mp3` or `.flac`), or the full path of a recording.
- OpenAI's voice names (`alloy`, `echo`, `nova`, ...) do not exist here; they, and no voice at all, mean the
  `tts_voice` setting, else no recording.
- An unknown voice is a `404` that says where to put the file.

## API

`POST /v1/audio/speech`, JSON, with the hub token as the API key:

| Field | |
|---|---|
| `input` | the text, up to 4096 characters (required) |
| `model` | see above |
| `voice` | see above |
| `response_format` | `wav` (default; 16-bit mono) or `pcm` (the same samples, raw little-endian). `mp3`, `opus`, `aac` and `flac` are refused with a `400`: no encoder is built in, convert the WAV with ffmpeg |
| `speed` | 0.25 to 4, default 1. llama.cpp's tool has no speed control, so the finished audio is time-stretched (WSOLA, pitch unchanged) |
| `language` | extension: ISO-639-1 code, for models that take one (`--tts-lang`) |
| `max_frames` | extension: limit on the generated audio (`-n`; how long a frame is depends on the model) |

The answer is the audio (`audio/wav` or `audio/pcm`). Errors: `400` bad request, `401` no token, `404` unknown or
missing model or voice, `500` the tool failed (the message has the end of its output), `501` `llama-tts` not found.

```bash
curl -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
     -d '{"model":"tts-1","input":"Hello there.","voice":"anna"}' \
     http://127.0.0.1:PORT/v1/audio/speech -o hello.wav
```

Each request starts one `llama-tts` process: the model is loaded, used and freed, one request at a time. That keeps
memory free between requests but costs the load time on every one.

## CLI and operation

```bash
model-mistress speak "Hello there." --out hello.wav [--model M] [--voice anna] [--speed 1.2] [--format pcm]
```

It runs the operation `speech.synthesize` (the same fields, plus `output`, the path on the hub's machine), so MCP and
`call` can use it too. The hub must be running.

## Tests

Request parsing, model and voice lookup, argument building, WAV handling and the speed change are unit tests, and
the hub's error answers are tested without a model. The test that runs a real model is ignored unless you give it one:

```bash
MM_TEST_TTS_MODEL=/path/Qwen3-TTS-12Hz-1.7B-Base-Q8_0.gguf [MM_TEST_TTS_VOICE=/path/speaker.wav] \
  cargo test -p model-mistress --features bundled-engine -- --ignored speaks_with_a_real_model
```

The model's `mmproj*.gguf` must be in the same folder.
