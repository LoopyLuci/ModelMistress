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
model-mistress call media.install '{"confirm": false, "url": "https://.../ffmpeg-n9.0-win64-lgpl.zip", "sha256": "<64 hex>", "size": 164000000}'
```

- Without `confirm: true` the answer (a `400`) says what would happen: the file, its size, where it comes from and the
  SHA-256 it must have. Nothing is fetched.
- **No build is pinned in this release.** A pin needs a file name, an address that never changes, a size and a SHA-256 that
  someone has downloaded and hashed, and the owner chooses the build (an LGPL one: no x264 or x265). Until then give
  `url` and `sha256` (an `https` address, or one on this machine), or `file` and `sha256` for an archive you downloaded
  yourself, or set `ffmpeg_dir`. `pin()` in `src/media/ffmpeg.rs` is the one place to fill in when the owner has chosen.
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
compute per second of video. That measurement was not repeated with the code in this branch (see "Not run").

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

## Not run, and open

See the changelog entry for the list of what was run and what was not. In short: no real ffmpeg was installed by this
code (no build is pinned), so the chat path with a real video, the install from a real archive and `media.probe` on a real
file have been tested against stand-in programs only; `--features whisper` and `whisper-vulkan` were not run on the owner's
card with the new code paths.
