# Speech to text

ModelMistress transcribes audio with whisper.cpp built into the binary. No Python, no separate server.

## Building

Speech is the cargo feature `whisper` (off by default, so the normal build stays small and needs no extra tools):

```bash
cargo build --release -p model-mistress --features whisper
# with the built-in llama.cpp as well:
cargo build --release -p model-mistress --features bundled-engine,whisper
```

It uses the `whisper-rs` crate, which compiles whisper.cpp with CMake. The build needs CMake, a C++ compiler and
libclang (for the bindings; set `LIBCLANG_PATH` to LLVM's `bin` folder if it is not found). On Windows build from a
Visual Studio developer prompt, or set `CMAKE_GENERATOR=Ninja`, as for `bundled-engine`. At run time nothing else is
needed. The build runs on the CPU. Without the feature the endpoints answer `501`.

## Models

A whisper model is a GGML file named `ggml-<name>.bin` (for example `ggml-base.en.bin`, `ggml-small.bin`,
`ggml-large-v3-turbo-q5_0.bin`; download them from `huggingface.co/ggerganov/whisper.cpp`). Put it in
`<home>/whisper`, `<home>/models` or any folder in `model_dirs`. The API name is the file name without `ggml-` and
`.bin`: `base.en`, `small`. A request can also give the full path of a model file.

A request that names no model, or `whisper-1` (what OpenAI clients send), gets the `whisper_model` setting if set,
else the only whisper model found, else the first of small, base, tiny. Models ending in `.en` are English only; they
cannot translate or take another language. The most recently used model stays loaded in memory.

## API

`POST /v1/audio/transcriptions` and `POST /v1/audio/translations` (the speech turned into English text), both
`multipart/form-data`, with the hub token as the API key:

| Field | |
|---|---|
| `file` | the audio (required) |
| `model` | a whisper model (see above) |
| `language` | ISO-639-1 code such as `de`; default: detected (transcriptions only) |
| `prompt` | text that guides spelling and style |
| `response_format` | `json` (default), `text`, `verbose_json`, `srt`, `vtt`, `ass` |
| `temperature` | 0 to 1, default 0 |
| `timestamp_granularities[]` | `word` times every word (`segment` is always there) |

The answers match OpenAI's: `{"text": ...}`; plain text; `verbose_json` with `task`, `language` (English name),
`duration`, `text` and `segments` (`id`, `seek`, `start`, `end`, `text`, `tokens`, `temperature`, `no_speech_prob`), and
with `word` also OpenAI's top-level `words` (`word`, `start`, `end`); SubRip; WebVTT; and Advanced SubStation Alpha
(`ass`; with `word` each word is a karaoke tag). Word times are whisper.cpp's token times grouped into words: estimates,
good to a fraction of a second.

Audio: WAV (PCM 8/16/24/32-bit, float 32/64-bit, any sample rate and channel count; converted to 16 kHz mono) and MP3 are
read by the hub itself. Any other audio or video file (Ogg, FLAC, M4A, WebM, MP4, MKV ...) is read through **ffmpeg** when
it is installed (`media.install`, [media.md](media.md)): the first audio stream is decoded to 16 kHz mono. Without ffmpeg
such a file is refused with a 400 that says so and how to get ffmpeg (a file named by path in `speech.transcribe`: 501).
Errors: `400` bad request or audio, `401` no token, `404` unknown model or no whisper model on the machine, `500` model
could not be loaded, `501` built without `whisper`.

A GPU build is the cargo feature `whisper-vulkan` (whisper.cpp's Vulkan backend): 59.8 times real time against 0.9 on the
CPU for the same model, measured on the RX 7900 XTX. It needs the Vulkan SDK to build, and a short `CARGO_TARGET_DIR` (the
Vulkan build of whisper-rs-sys fails when the build path is long). `media.components` says which build runs.

```bash
curl -H "Authorization: Bearer $TOKEN" -F file=@talk.wav -F model=base.en \
     -F response_format=srt http://127.0.0.1:PORT/v1/audio/transcriptions
```

## CLI and operation

```bash
model-mistress transcribe talk.wav --model base.en [--language de] [--translate] [--words] [--format text|json|verbose_json|srt|vtt|ass]
```

It runs the operation `speech.transcribe` (file path on the hub's machine, same options; `words: true` times every word),
so MCP and `call` can use it too. The file may be a video: `model-mistress transcribe meeting.mp4 --format ass --words`
(needs ffmpeg). The hub must be running.

## Tests

Request parsing, audio decoding, formatting, model lookup and the error answers are unit tests. The test that runs a
real model is ignored unless you give it one:

```bash
MM_TEST_WHISPER_MODEL=/path/ggml-base.en.bin MM_TEST_WHISPER_WAV=/path/speech.wav MM_TEST_WHISPER_EXPECT="a word said" \
  cargo test -p model-mistress --features whisper -- --ignored transcribes_real_speech
```
