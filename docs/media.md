# Media: video and audio in chat, ffmpeg, speech to text on any container

Phase 1 of the media plan. Four parts, each usable without the others:

1. **ffmpeg as a setup component** (`media.components`, `media.install`): nothing is downloaded unless the owner says so.
2. **Video and audio in chat**: `video_url`, `input_video` and `input_audio` parts reach llama-server, which decodes the
   video with ffmpeg and feeds the frames (and the sound) to the model through its projector.
3. **`media.probe`**: what a media file holds.
4. **Speech to text on any container** (MP4, MKV, M4A, Ogg, FLAC, WebM ...), the `ass` subtitle format, word times, and
   a Vulkan build of whisper.

Every part says what is missing and how to get it instead of failing in the middle: without ffmpeg a feature that needs it
answers `501` (or lists the model's modalities without `video`) with the sentence "ffmpeg is not installed: `media.install`
with confirm: true installs the pinned build ...".

## The ffmpeg component

`ffmpeg` and `ffprobe` run as separate processes (the hub links no media library). They are found in this order:

| Where | Used |
|---|---|
| the `ffmpeg_dir` setting, else `MM_FFMPEG_DIR` | as it is: the folder the owner points to |
| `<home>/engines/ffmpeg/` | after its files are hashed against `install.json` (see below) |
| `PATH` | the first folder that holds both programs |

`media.components` (also `GET /v1/media/components`) reports each component: where ffmpeg is, its version and licence
(`LGPL`, `GPL` or `nonfree`, read from the build's `configuration:` line; a GPL build is reported, not refused: what runs
on the owner's machine is the owner's call), the pinned build, the speech component (`built`, `build` = `cpu`, `vulkan` or
`none`, the models found) and what each projector model can be given. It also says whether `video_input` and
`any_container_transcription` are ready, and if not why.

### `media.install`

```bash
model-mistress call media.install '{"confirm": false}'      # says what the pinned build is and what it would fetch
model-mistress call media.install '{"confirm": true, "file": "C:/dl/ffmpeg-....zip", "sha256": "<64 hex>", "wait": true}'
```

- Without `confirm: true` the answer (a `400`) says what would happen: the file, its size, where it comes from and the
  SHA-256 it must have. Nothing is fetched.
- **The pinned build** (Windows x64 only; nothing is pinned elsewhere) is BtbN's FFmpeg-Builds release n8.1, LGPL, shared:
  `ffmpeg-n8.1.3-41-ga8029b6528-win64-lgpl-shared-8.1.zip`, 81,544,084 bytes, SHA-256
  `bb63313643f91add6ab97f546c216b07378476f4953f983c9e1c8875f4ff9558` (the programs and their DLLs). Its address is a
  daily-build page whose files stay up for 14 days, so it will stop working: keep the zip and install it with `file` and
  `sha256`, or pick another build and change `pin()` in `src/media/ffmpeg.rs`. `url` and `sha256` (an `https` address, or one
  on this machine) install any other build. n9.0.2-982-g836df4e858 (`...win64-lgpl-shared-9.0.zip`, 77,786,993 bytes, SHA-256
  `ab4b0718084667b20f8f83b6c32e3f5d5c1b14274fffdb3b24da0eb62a7b1460`) was installed and measured beside it (below).
- The archive's hash is checked before anything is unpacked; a mismatch (`422 hash_mismatch` as a job error) installs
  nothing. The unpacked programs are run once (`-version`) before they replace anything.
- The result goes to `<home>/engines/ffmpeg/` (the player, `ffplay`, is left out). An earlier install is kept beside it as
  `ffmpeg.old-<time>`, never deleted. `install.json` records the size and SHA-256 of every installed file; they are
  checked again the first time a file is used and whenever its size or time changes. A file that was swapped, or a folder
  with no record, is an **error that says how to put it right** (`media.install` with `force: true`), never a silent use.
- It runs as a job (`job.get` shows the download); `wait: true` answers when it has finished. Running it again with the
  same archive says `already installed` unless `force: true`.

## Video and audio in chat

A chat message takes OpenAI's `image_url` and `video_url` parts and llama-server's `input_video` and `input_audio`:

```json
{"model": "qwen3-vl-8b", "messages": [{"role": "user", "content": [
  {"type": "text", "text": "What is on the slides?"},
  {"type": "video_url", "video_url": {"url": "file:///C:/clips/meeting.mp4"}}]}]}
```

- **Local files.** A llama-server reads a local file only as `file://<name>` under the folder it was started with
  (`--media-path`); a bare path is read as base64 and fails (`HTTP 500`, measured). The hub starts every model that has a
  projector with `--media-path <home>/media`, and `--video-ffmpeg-dir <folder>` when ffmpeg is there, each only if the
  server lists the flag. The owner's own `--media-path` / `--video-ffmpeg-dir` (in `extra_args`) win.
- **The owner names a file anywhere on the machine** (`file:///C:/clips/a.mp4`, `C:\clips\a.mp4`, `/home/me/a.mp4`; `%20`
  and the like are decoded in `file://` URLs): the hub links it (else copies it) into `<home>/media/staged/` and passes the
  server the `file://staged/<name>` of the copy. The same file is staged once; copies nobody asked for in 24 hours are
  removed the next time one is staged. A file that is not there is a `400` that names it.
- **A peer key's request is never rewritten.** It can name only what is already in `<home>/media`, so a peer cannot read a
  file of the owner's machine through the chat route. (`/v1/media/*` and the `media.*` operations are the owner's.)
- **Base64** works up to 128 MB of request (axum's own limit, 2 MB, refused even a short clip; that limit is raised on
  `/v1/chat/completions` only). A longer file is named by path instead.
- **What a model can be given** is in `model.list` and `/v1/models` as `input_modalities`: `text`, then `image` and
  `audio` from the projector's own flags (`clip.has_vision_encoder`, `clip.has_audio_encoder`), then `video` when the
  model sees images and ffmpeg is there and the server takes `--video-ffmpeg-dir`. When video is possible but not ready,
  `video_note` says why.

Measured before this was built (the plan's spike, a 74 s screen recording of a meeting, native video with ffmpeg):
Qwen3-VL-4B, Qwen3-VL-8B and Gemma 4 E4B each answered the five questions about it correctly, at about one second of
compute per second of video. The same kind of check was repeated with this code on one model (see "Run with real builds").

## `media.probe`

`media.probe` (`POST /v1/call/media.probe {"file": ...}`, and `POST /v1/media/probe`) runs ffprobe and answers the
container (format, duration, size, bit rate), every video stream (codec, size, frame rate, frames, rotation, cover art),
audio stream (codec, sample rate, channels, language) and subtitle stream, and `has_video` / `has_audio`.
`POST /v1/media/probe` takes either JSON `{"file": "<path on the hub's machine>"}` or a multipart upload with a `file`
field (up to 512 MB; kept in `<home>/media/tmp` while ffprobe reads it, then removed). Needs ffmpeg.

## Speech to text on any container

`/v1/audio/transcriptions`, `/v1/audio/translations` and `speech.transcribe` read WAV and MP3 themselves, as before. Any
other file (MP4, MKV, WebM, M4A, Ogg, FLAC ...) goes through ffmpeg when it is installed: the first audio stream, as 16 kHz
mono samples. `speech.transcribe` names its file by path and ffmpeg reads it where it is, so a long recording is not held in
memory; an upload is written to `<home>/media/tmp` for ffmpeg (it needs to seek in an MP4) and removed. Without ffmpeg the
answer says what is missing (`501` for a path, the usual `400` with the install hint for an upload).

What keeps ffmpeg from being sent somewhere it should not go:

- the file is probed first and only plain audio and video containers are decoded (not playlists, manifests, `concat`,
  image sequences, devices or raw streams: those can name other files and addresses);
- ffmpeg and ffprobe get `-protocol_whitelist file`, so no network;
- at most 4 hours are decoded; ffprobe is stopped after 60 s and ffmpeg after `120 s + 1/10 of the length`, and a tool that
  writes more than it should is stopped;
- the upload copy is removed, also when the decode fails.

New in the answer:

- **`ass`** as a response format (`response_format=ass`, `format: "ass"`): Advanced SubStation Alpha subtitles. With word
  times each word is a karaoke tag (`{\k<centiseconds>}`), so a player lights each word as it is spoken.
- **Word times**: `timestamp_granularities[]=word` (OpenAI's field) or `words: true` on the operation. `verbose_json` then
  has OpenAI's top-level `words` list (`word`, `start`, `end`). They are whisper.cpp's token times grouped into words:
  estimates, good to a fraction of a second, not forced alignment.
- **`whisper-vulkan`**, a Cargo feature that builds whisper.cpp with its Vulkan backend (`cargo build --release -p
  model-mistress --features whisper-vulkan`; implies `whisper`). Measured on the RX 7900 XTX with whisper-rs 0.16: 59.8
  times real time against 0.9 on the CPU for the same model. The Vulkan build of whisper-rs-sys fails when the build path is
  long (the shader compile): use a short `CARGO_TARGET_DIR`. `media.components` says which build runs (`build`).

## Run with real builds (2026-11-27)

Both builds were installed with `media.install` from their zips (nine seconds each) into scratch homes, and a scratch hub
with the pinned-engine `llama-server` (llama.cpp 7fee178) ran the checks on the RX 7900 XTX. The owner's hub was not touched.

| | n8.1.3 (pinned) | n9.0.2 |
|---|---|---|
| Licence read from the build | LGPL (`--enable-version3`, no `--enable-gpl`, no `--enable-nonfree`) | the same |
| `media.probe` of the 32 s MP4 | same fields | same, except ffprobe reports the AAC `initial_padding` as 1024 (n8.1: 0), which the hub does not show |
| Decode 322 frames of 720p, 5 runs (median) | 0.12 s, about 265 times real time | 0.12 s, about 280 times |
| Audio to 16 kHz mono, ffprobe | 0.04 s, 0.03 s | the same |
| PCM that ffmpeg wrote | identical to n9.0.2's, byte for byte | identical |
| `speech.transcribe` of the MP4 (whisper `large-v3-turbo-q5_0`, CPU, debug build, model load included) | 42 s for 32 s of audio; text identical to the spoken script | 41 s; the same text |
| Chat, Gemma 4 26B, `video_url` of the local MP4, 13,116 prompt tokens | 5 of 5 answers correct | 5 of 5 |
| Follow-up answers (prompt cached) | 3.5 to 5.2 s | 2.9 to 5.3 s |
| Generation over 900 tokens | 82.2 tokens/s | 84.8 tokens/s |

The two builds behaved the same. What the run found:

- **Gemma 4 26B's projector crashes this llama-server on the GPU**: the process died while encoding, for a single picture as
  well as for the video (the same with 20k and 32k of context, with 6.7 GB of VRAM free), so it is the model, projector and
  server, not ffmpeg or the hub (the hub staged the file and the server read it). With `--no-mmproj-offload` it runs, but the
  projector then works on the CPU: the first answer about the video took **16 minutes** (965 s and 957 s), the later ones seconds.
  The models of the plan's spike (Qwen3-VL, Gemma 4 E4B) are no longer on this machine and nothing was downloaded, so the
  exit check ran on this model. Use `extra_args` (`--no-mmproj-offload`) per model until a model whose projector runs on the GPU is used.
- **llama-server's default frame rate is too high for long context**: the 32 s clip came to 52,595 tokens at the default and
  was refused as over a 32,768 context; with `--video-fps 1` (the setting `extra_args`, as in the spike) it is 13,116 tokens.
  The hub now passes `--video-fps 1` itself when ffmpeg is there and the server lists the flag; the owner's own
  `--video-fps` (in `extra_args`) wins.
- The speed figure is 900 generated tokens; the first-answer time is the CPU projector and is not a speed of the hub.

## Not run, and open

See the changelog entry for the list of what was run and what was not. In short: `whisper-vulkan` was not built, and the
GPU speed of whisper and a vision model whose projector runs on the GPU were not measured with this code. The tests use
stand-in programs; the run above used real builds, a real model and a real clip.
