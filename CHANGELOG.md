# Changelog

## Unreleased

### Misty: the optimal-agents spec applied (budgets, a stronger gate, a context that fits, notes)

The spec is `docs/misty/optimal-agents-spec.md` (written from the owner's notes on agent design); the mapping of each
requirement to what Misty does, with the tests, is `docs/misty/optimal-agents-map.md`. Checked with a scripted model, not
yet with the real Bonsai models.

- **A run that runs out ends with an answer, not an error.** At `max_steps` or the new `max_tool_calls` the run makes one
  last call with no tools and the model says what it found and what is missing; at `run_timeout_s` (the hard limit) no
  further model call is made and the host writes that note from what the run did. The run is `done` with `partial` set to
  `steps`, `tools` or `time` (in `misty.ask`'s result and the `final` event), and its text starts "(Stopped early: ...)".
  A pipeline step that ends this way fails, so a later step does not build on it. Before, the run was an error with no
  summary.
- **The gate counts tool calls.** A reply may carry `max_calls_per_reply` calls (default 4) and a run may make
  `max_tool_calls` (default 16, refused calls included); the rest are not run and the model is told. Cancel is now also
  checked between the calls of one reply. A caller that names its tools (`tools` on a request) gets a refusal for any
  other, even when the policy would allow it.
- **The prompt fits the context.** Nothing used to trim a run's messages, so a few large tool results could push a
  8192-token context over its limit. Before each call the prompt is brought under `context_tokens` (default 8192, less
  the reply and the tool definitions): earlier tool results become a note, then the oldest turns go, then the latest
  results are cut. A `context` event says so.
- **Text from outside cannot close its own label, and order-like text is shown to the owner.** A tool result can no
  longer contain `<tool_result` or `</tool_result`. Wording that reads as orders ("ignore your instructions", a fake
  `system:` line, "do not tell the user", "run the following command") is named in the tool event (`flags`), in a warning
  after the result the model reads, and as a "Look twice" line beside every approval the run asks for afterwards
  (dashboard and CLI; the approval event and `misty.approvals` carry `warning`). It is a heuristic list and never blocks;
  the approvals still stop a write, a hub change or a network call.
- **Per-tool policy.** `policy.tools` (in Misty's settings and in an environment) gives one tool its own `auto`, `ask` or
  `deny`, which wins over its class; a name that is not a tool is refused when saved. An environment that sets a class
  replaces Misty's per-tool entries for it. `misty.tools.list` shows each tool's own policy. (The `denied` message names the
  tool when its own entry is what refused it.)
- **The bigger model is told why.** When an answer is rejected and the run retries one role up, the system prompt says why
  ("rejected: empty answer").
- **`remember`**, a tool of class `write` (so the owner approves each one by default), keeps one short fact in the document
  index; it comes back by retrieval. A note that gives orders is refused.
- **Ask first when it matters.** The system prompt tells the model to ask one short question when a request is unclear and
  a wrong guess would change, delete or run something, and otherwise to work and say what it assumed. A prompt rule only.
- Docs: `DESIGN.md` section 3.3 and 3.4 follow the code; it said network tools default to `ask`, and the code (and now the
  document) says `deny`.

### Media: video and audio in chat, an ffmpeg component, speech to text on any container

- ffmpeg is a setup component, not a download that happens by itself. `media.components` (and `GET /v1/media/components`)
  says where ffmpeg and ffprobe are (the `ffmpeg_dir` setting, `MM_FFMPEG_DIR`, `<home>/engines/ffmpeg/`, then PATH), their
  version and licence (a GPL or nonfree build is reported, not refused), and what each projector model can be given.
  `media.install` (owner only) puts a build into `<home>/engines/ffmpeg/`: without `confirm: true` it answers what it would
  fetch, from where, how big, and the SHA-256 it must have; the archive's hash is checked before it is unpacked, the
  programs are run once (`-version`) before they replace anything, every installed file's size and hash go into
  `install.json` and are checked before the file is used (a swapped file or a folder with no record is an error that names
  the fix, never a silent use), and an earlier install is kept as `ffmpeg.old-<time>`. Runs as a job; `wait: true` waits.
  New setting `ffmpeg_dir`. See `docs/media.md`.
- **The pinned build** (Windows x64) is BtbN's FFmpeg-Builds n8.1, LGPL, shared: `ffmpeg-n8.1.3-41-ga8029b6528-win64-lgpl-shared-8.1.zip`,
  81,544,084 bytes, SHA-256 `bb633136…f9558`. Its download address is a daily-build page that keeps files 14 days, so
  keep the zip and install with `file` + `sha256`, or pin another build in `pin()`. `url` + `sha256` and `file` + `sha256`
  install any build. Installed from the zip in this change, and n9.0.2 beside it; the hub never downloads by itself.
- Video and audio in chat. A chat message's `video_url`, `input_video`, `input_audio` and `image_url` parts reach the
  model's llama-server. A model that has a projector is started with `--media-path <home>/media` and, when ffmpeg is there,
  `--video-ffmpeg-dir <folder>`, each only if that llama-server lists the flag and unless the owner's own arguments set it.
  A llama-server reads a local file only as `file://<name>` under `--media-path` (a bare path fails with HTTP 500, measured),
  so the owner's own requests (`/v1/chat/completions`, `chat.complete`) may name a file anywhere on the machine
  (`file:///C:/clips/a.mp4`, a plain path, `%20` decoded): the hub links or copies it to `<home>/media/staged/` and passes
  `file://staged/<name>`. Copies nobody asked for in a day are removed. A peer key's request is never rewritten, so a peer
  cannot read a file of the owner's machine through chat. `/v1/chat/completions` takes up to 128 MB of body (axum's 2 MB
  refused a short clip as base64).
- `model.list` and `/v1/models` carry `input_modalities` (`text`, `image`, `audio`, `video`): image and audio from the
  projector's own flags (`clip.has_vision_encoder`, `clip.has_audio_encoder`), video when the model sees images, ffmpeg is
  there and the server takes `--video-ffmpeg-dir`; `video_note` says what is missing when video is possible but not ready.
- `media.probe` (and `POST /v1/media/probe`, JSON `{"file"}` or a multipart upload up to 512 MB): what ffprobe says about a
  file, as the container (format, duration, size, bit rate) and every video, audio and subtitle stream (codec, size, frame
  rate, rotation, sample rate, channels, language, cover art). Owner only; ffprobe gets `-protocol_whitelist file`.
- Speech to text reads any container. `/v1/audio/transcriptions`, `/v1/audio/translations` and `speech.transcribe` keep
  reading WAV and MP3 themselves; any other audio or video file goes through ffmpeg when it is installed (the first audio
  stream, 16 kHz mono), and `speech.transcribe` leaves a file named by path on disk for ffmpeg (a long recording is not held
  in memory). Only plain audio and video containers are decoded (not playlists, `concat`, image sequences, devices or raw
  streams), ffmpeg gets only the `file` protocol, at most 4 hours are decoded, and every process has a time limit. Without
  ffmpeg the answer says how to get it (`501` for a path; the usual `400` for an upload).
- New response format `ass` (Advanced SubStation Alpha) and word times: `timestamp_granularities[]=word` on the OpenAI
  route, `words: true` / `--words` on `speech.transcribe` / `model-mistress transcribe`. `verbose_json` then has OpenAI's
  top-level `words`; `ass` writes each word as a karaoke tag. The times are whisper.cpp's token times grouped into words:
  estimates, good to a fraction of a second.
- Cargo feature `whisper-vulkan` (whisper.cpp's Vulkan backend; implies `whisper`); `media.components` and
  `speech::BUILD` say which build runs (`vulkan`, `cpu`, `none`). The Vulkan build of whisper-rs-sys fails when the build
  path is long: use a short `CARGO_TARGET_DIR`. Built and run on the RX 7900 XTX: the 32 s MP4 transcribes in 1.2 to 1.4 s
  once the model is loaded (same text as the CPU). The card's cooperative-matrix kernels end the process at the first encode
  (exit code 0xC0000409, no message), so the hub sets `GGML_VK_DISABLE_COOPMAT=1` while the model loads (unless the owner
  set it); `MM_WHISPER_LOG=1` makes whisper.cpp and ggml print to stderr.
- Measured before this was built (the media plan's spikes, not repeated with this code): a 74 s screen recording of a meeting
  was answered correctly (5 of 5 questions) by Qwen3-VL-4B, Qwen3-VL-8B and Gemma 4 E4B through llama-server's native video
  input, at about one second of compute per second of video; whisper.cpp through whisper-rs 0.16 with Vulkan ran 59.8 times
  real time against 0.9 on the CPU on the RX 7900 XTX.
- Run on this machine with real builds, a real model and a real clip (a scratch hub, the owner's hub untouched): ffmpeg
  n8.1.3 and n9.0.2 installed from their zips, both read as LGPL; `media.probe`, decoding (322 frames of 720p in 0.12 s) and the
  16 kHz audio the same on both; `speech.transcribe` of a 32 s MP4 (whisper large-v3-turbo-q5_0 on the CPU) gave the spoken
  script exactly in 41 to 42 s; a Gemma 4 26B chat with `video_url` of the local MP4 answered 5 of 5 questions on both builds
  (900 generated tokens at 82 and 85 tokens/s). Two findings: that model's projector crashes the pinned llama-server on the
  GPU (for a single picture too), so it ran with `--no-mmproj-offload` and the first video answer took 16 minutes; and
  llama-server's default frame rate gave 52,595 tokens for the 32 s clip, over a 32k context, so `--video-fps 1` was given
  in `extra_args`. The hub now passes `--video-fps 1` itself (with ffmpeg present, to a server that lists the flag; the
  owner's own `--video-fps` wins).
- Tests for this change: with `--features whisper` the 35 speech, audio and media tests pass too (the test binary links
  only against the MSVC 14.51 libraries here: whisper.cpp was compiled with its headers). `cargo test --lib` (807 tests: 800 pass, 6 ignored, and `documents::tests::a_pdf_gives_its_text`
  fails in this worktree because its PDF fixture was checked out with Windows line endings, which breaks the file's byte
  offsets; the file is not touched here and `*.pdf` is not marked binary in `.gitattributes`), `rustfmt`, and clippy with
  `-D warnings` on this change's files (three other lints, in `misty/ops.rs`, `batch.rs` and `misty/agent.rs`, come from the
  newer toolchain and are not touched). The tests use stand-in ffmpeg, ffprobe and llama-server programs (the install from a stored
  zip with a right and a wrong hash, a swapped file, the listings, the chat path through a stand-in server that records what
  it was sent, a peer's request left alone, the ass and words formats). Not run: a vision model whose projector runs on the GPU
  (Qwen3-VL and Gemma 4 E4B are not on the machine), the new routes with a browser or a real client.

### System awareness: the hub watches the PC and yields to it, live

- Every two seconds the hub reads how pressed the machine is: CPU used by *other* programs (the machine's total less the hub's
  own tree), free memory, Windows commit headroom, free video memory, temperatures, GPU and drive load, and whether a game, a
  full-screen program or a presentation has the screen. Level: `calm`, `busy`, `strained` or `critical`; it rises after two
  looks and falls one step after five, so a flicker changes nothing. `system.status` lists every reason with its numbers.
- What the governor does, by mode (`off`, `gentle`, `balanced` (default), `strict`): workers the hub started (Forge, image,
  quantize, ffmpeg and the like) run below normal always, go to idle priority plus Windows efficiency mode when strained,
  and are paused after `pause_after_s` of critical (at most `max_pause_s`); a model server drops to below normal when strained
  and is never paused; any other process the hub started is lowered a step later. It only lowers, only touches descendants of
  the hub (named by pid *and* start time), puts back exactly what it changed (level falls, mode off, hub stops) and does not
  fight a priority someone else changed. Console hosts and other helpers the OS starts for a child are left alone. A process
  counts as the hub's child only when its start time is known and not older than its parent's: Windows reuses a parent pid
  after the parent exits, and on the live hub an `adb.exe` that predates the hub was lowered through such a pid (found there,
  fixed, four tests).
- Heavy jobs (`forge`, `factory`, `quantize`, `image`) do not start while the machine is strained or worse: the job exists
  at once, says why it waits, can be cancelled, and `system.run_now` starts it anyway. `max_wait_s` (default 0: as long as it
  takes) gives up waiting. Downloads, installs and Misty's own jobs never wait.
- When memory, commit or video memory is the short resource and the machine is strained, the hub unloads the model that has
  been idle longest (`shed_idle_s`, default 120 s, one per ten seconds, never one with a request in progress; a `keep_alive:
  -1` model only at critical). `shed = false` turns it off.
- `system.status`, `system.config.get`, `system.config.set`, `system.run_now` (owner only; `system.status` is also a core MCP
  tool, so the core set is thirteen); `model-mistress system [status|watch|mode|config|run-now]`; settings in
  `<home>/system.toml`, taken in on edit; `machine.status` carries a short `pressure` block. See `docs/system-awareness.md`.
- SystemWatchdog is optional and adds GPU load, temperatures, drive activity, video memory and the foreground program, read
  from Watchdog's new loopback feed (`GET http://127.0.0.1:7781/pressure`, read-only, Host-checked). A feed that is absent
  costs nothing (the hub backs off to one look per 30 s); one that stops publishing is reported `stale` and its readings are
  dropped. In return Watchdog's own action engine and core balancer skip processes descended from the hub. Watchdog changes
  are in its repository, uncommitted.
- Not done: no CPU affinity or thread budget; no per-process GPU priority (Windows has none; the hub can only hold heavy
  jobs back and shed models); on Linux and macOS the hub looks, waits and sheds but changes no process priority.
- Run on this machine: a scratch hub (own home and port, the live hubs untouched) with a real `forge.run` job through a
  stand-in worker, a real CPU-burning Python child. Calm: below normal. Critical: idle plus efficiency mode, paused after 20 s,
  resumed after 121 s; stepped down critical, strained, calm; mode off put both processes back to normal while they ran; a
  heavy job waited at strained, was cancelled while waiting, and another was started with `run-now`; Watchdog's real feed class
  on a scratch port served synthetic readings (hot GPU made the hub strained, a silent feed went stale, a changed URL connected
  at once). The scratch run also found two faults, both fixed with tests: a console host was lowered like a program, and a
  changed `watchdog_url` waited out the old address's backoff.
- Not run: a real model server being lowered or a real model being shed (the scratch hub had no model; the tests use
  stand-in instances); a Forge run on the GPU; the installed Watchdog feeding real readings (its code is changed but the
  running program was not restarted); the live hubs.
- `src/sysaware/` (`level`, `policy`, `probe`, `feed`, `actuate`, `governor`, `ops`, `cli`, `mod`), `Engine::shed_one`, the
  admission gate in `Jobs::start_task`; 71 tests including a real-process loop test.

### Data-broker opt-outs: f*ck_data_brokers, pinned and driven from the hub

- `model-mistress privacy …` and the `privacy.*` operations drive [f*ck_data_brokers](https://github.com/gary23w/f-ck_data_brokers)
  (MIT): public broker registries in, legal deletion and opt-out letters out (CCPA, GDPR, UK GDPR, PIPEDA, Quebec Law 25,
  LGPD), replies read, silent brokers chased. It never sends an ID, clicks a link or solves a CAPTCHA.
- The tool is **pinned**: commit `528fb023`, SHA-256 `dce00f90…aa093`, 85,839 bytes. `privacy install` downloads that one
  file only when told to (`confirm: true`; or installs a copy you fetched with `--file`), keeps it only if the hash matches, and
  checks the hash again before every call. It runs as `python -I` with a cleared environment (none of the hub's tokens reach
  it) and its state under `<home>/privacy/`, one call at a time.
- Nothing is sent without the owner's count: `privacy send` first lists who would get an email, and sends only when called
  again with `confirm_count` equal to that number (the CLI asks you to type it); a queue that changed in between is refused.
  Sending from the hub needs `FDB_SMTP_PASS` (an app password) in the hub's environment, handed to the tool for that one call
  and never stored, logged or returned; without it the send page (one button per letter, your own mail program) needs no password.
- Owner only: a peer key cannot reach it and Misty has no tool for it. See `docs/privacy.md`.
- Not done: no page in `/ui`; the count `privacy send` confirms is read from the tool's state file in the pinned version's
  format, so moving the pin means checking that reader.
- Run on this machine, in a scratch home with a placeholder identity and `.invalid` addresses: install refused without
  confirm and for a wrong file (hash mismatch, nothing written), installed from a local copy of the pinned file, refused after
  the file was altered until `--force` put it back, setup, two brokers added, two letters drafted, the send preview listed both,
  a wrong count and a missing password were refused. Nothing was sent and nothing was downloaded.
- Not run: `brokers import` (it reads the public registries over the network), a real send through a mail server, reply
  parsing on a real broker's mail, forms.
- `src/privacy/` (`mod`, `ops`, `cli`, `tests`), 14 tests.

### Batch jobs: prompts in order, answers written as they finish

- `batch.submit` with `model`, `prompts` (up to 10,000), and optionally `system`, `name`, `max_tokens` (1 to 8192, default
  512) and `concurrency` (1 to 8, default 2). It returns the job's id at once and runs in the background. Each answer goes to
  `<home>/batches/<id>/results.jsonl` as it finishes, synced to disk. A failed item is recorded with its error, and the job goes on.
- `batch.get` (state and progress), `batch.list` (running, done, cancelled or all), `batch.results` (a page of finished items,
  `from` and `limit`, with `next`), and `batch.cancel` (a running job only: the items in progress finish, no further one starts).
- A restart runs again every batch that was running, for the items with no answer. A line cut off by the crash is dropped.
- Owner only, and in `mcp --all-tools` rather than the core set. Not done: peer keys cannot submit a batch; there is no
  per-key quota; `concurrency` is per job, not across jobs; there is no page in `/ui`. Not run: a batch of real answers from
  a loaded model (the tests run items through a hub with no model server, and check the restart and the file handling).
- `src/batch.rs`, seven tests.

### Approval queue: a peer key asks, the owner approves

- A peer key cannot load a model, but it can ask. `POST /v1/requests` with `{"model", "reason"}` queues a request and answers
  `202`; nothing loads. A key that asks again for a model it already waits for gets the same request, and at most 20 requests
  wait per key. The peer MCP session has `model_request` and `request_status` (`GET /v1/requests/:id`, the key's own requests only).
- The owner lists the queue with `request.list` (pending by default). `request.approve` loads the model, as `model.load` does; if
  the load fails, the request stays pending and the error says why. `request.deny` refuses, with a reason the key can read. A
  decided request cannot be decided again, so a denied request never loads its model.
- The queue is kept in `<home>/requests.json`: every pending request and the newest 200 decided ones (`src/requests.rs`).
- Not done: an approval page in `/ui` and a badge for pending requests. Not run: a real approve that loads a model (the tests
  cover the queue, the refusals and the denied-then-approve path, which never loads).

### MCP: a core tool profile

- The owner's MCP session (`model-mistress mcp`) offers thirteen core tools: `service_status`, `system_status` (how pressed the machine is, see "System awareness" above), `model_list`, `model_loaded`,
  `model_info`, `model_plan`, `model_load`, `model_unload`, `chat_complete`, `logs_tail` and the approval queue (`request.list`, `request.approve`, `request.deny`). Before, it offered every hub
  operation (about 94), which cost an agent context on every turn. A tool outside the set is refused as a tool that does not
  exist.
- `model-mistress mcp --all-tools` offers every operation again: downloads (`model_search`, `model_pull`), deletes, settings,
  Misty and the fabric. It cannot be combined with `--peer`.
- The core set is `CORE_OPS` in `src/client.rs`; a test checks that each name is an operation the hub lists
  (`the_core_profile_offers_the_core_tools_and_all_offers_every_operation`).
- An MCP client that was set up before this change, such as the Claude Code plugin's `.mcp.json`, now gets the core set. To keep
  the full list, start the server with `--all-tools`. See `docs/claude.md`.

### Desktop app: Misty, chat and speech

- Misty ratings: each reply can be rated Good, Bad, No tool needed or Should have used a tool, or corrected. Ratings are
  kept in Misty's database (`misty.rate`, `misty.feedback.list`, `misty.feedback.forget`). "No tool needed" and "Should have
  used a tool" route similar questions, and a correction is given to her as a note for similar questions. A Good or Bad rating routes nothing, so it does not switch tools on or off (`src/misty/feedback.rs`, `src/misty/agent.rs`).
- Learning tab on Setup and status: the rating counts, an export as chat-format JSON lines (`misty.training.export`), and
  forgetting one rating or all of them.
- Chat: a message written during a reply waits and is sent after it; "Jump to latest" when scrolled up; Dictate (the
  microphone into the message box); "Read aloud" under each reply.
- Speech page: dictation works. Recordings are written as WAV in the browser before upload, because the hub reads WAV and
  MP3 but not WebM (`frontend/src/wav.ts`).
- Chat, dashboard and native window: top-k and seed; text, source and PDF files attached to a message (a PDF's text comes
  from the hub); a camera button for photos; steering a reply mid-sentence (the text so far is kept, the note goes next);
  and Live, a conversation by voice (each sentence is written down and sent, the reply is read aloud a sentence at a time,
  talking over it stops it, and a spoken correction is saved for Misty). Live takes the reply out of the microphone where it can: the native window measures the delay from its speaker to its
  microphone from the sound and runs an echo filter, and it lowers steady background noise; the web asks the browser for
  echo cancellation and says so when the browser does not apply it. Not yet checked on a real microphone and speaker.
- Misty page: a message written during a reply waits and is sent after it, on the dashboard and the native window.
- Models, dashboard and native window: "Import model file" copies a `.gguf` into the models folder with progress; the
  speed test times this machine and each installed chat model, then suggests the fastest, the one that keeps up with
  reading, and the best that fits in memory (`system.bench`).
- Native window (`gui/`): brought to the same features as the dashboard for these pages: the Chat page (Live, queue,
  steer, attachments, camera, dictation, read-aloud, top-k, seed), the Misty page (queue, steer, ratings, "No tools", What
  she learns) and the Models page (import, speed test). Its tests run offscreen with fake sound and a fake hub.
- Parity with the Android app: `docs/android-parity.md`. Open: echo cancellation on a real microphone and speaker (checked offline only; a headset is
  advised), steering a reply gives no rating for the cut reply, and the web Live cannot interrupt a reply early.
- Tests: `cargo test` passes on every target (651 library, 2 binary and 16 desktop-app tests; 6 ignored), the frontend tests (vitest, 222 tests; `vue-tsc` and `vite build` clean),
  and the native window's pytest suite (169 tests and 1 skipped; with `MM_HUB_EXE` set, the real-hub test runs too). Not run on a real microphone,
  camera, speaker or large model file.

### Fixes from the review of this branch

- A request that the fabric served for this hub is counted in the peer key's usage when it succeeds, as a failed one already
  was (`src/hub.rs`, `passthrough`). The tokens are counted too, from the answer (see Fabric below).
- The build script trusts only this checkout for git (`src/build.rs`), not every directory on the machine.
- An approval holds a claim on its request while the model loads: a deny that comes in meanwhile is refused (409
  `approval_in_progress`), and a second approve waits. Before, a deny could land in the middle and the model still loaded.
- A request belongs to the peer key's id, not its name, so two keys with one name do not share requests (`src/requests.rs`).
- `batch.submit` refuses a model this hub does not serve before it writes the job, and refuses an option of the wrong type
  (`concurrency: "8"`, `max_tokens: -1`) rather than taking the default.
- `batch.cancel` and a job's final write share a lock, so a job that finishes as it is cancelled is never overwritten.
- `batch.get` and `batch.list` count only the lines appended since the last call, not the whole results file. `batch.results`
  reads up to the end of the page it returns.

### Android app (`android/`, version 0.1.0)

- Live conversation: the phone listens while the model speaks, so talking over it stops it. The phone's own voice is matched
  against the last 160 words it said, so it is not taken as the owner's. A finished sentence waits about 1.5 seconds with
  Cancel send, or "cancel" spoken, before it is sent. Edit before send keeps it in an editable box until Send now. Listen
  pauses the microphone without ending a reply. Live stays open when the phone rotates.
- Voice corrections: a sentence that starts with "correction", "that's wrong" or "not quite" is saved against the last reply
  for Misty, not sent as a message. The Misty panel counts the training examples and exports them as chat-format JSON lines.
- Default model is LFM2-2.6B-Q4_0 (1.4 GB).
- Not yet checked on the phone: Send now, corrections, the training export, and the echo matching over a full session.
  See `android/docs/FEATURE_MAP.md`.

### Fabric: ModelMistress in a ComputeFabric

- New `machine.status` operation: each GPU's free memory, what models may use of it (`vram_usable_mib`, held to
  `vram_reserve_mib` and the Windows commit), free RAM and what is running. ComputeFabric reads it to decide where a model
  fits.
- New `fabric.*` operations (`fabric.status`, `fabric.nodes`, `fabric.models`, `fabric.route`, `fabric.set`) and
  `<home>/fabric.json`: a hub joined to a ComputeFabric passes a request it cannot serve (no room for the model, or no free
  slot with `fail_fast_busy`) to the fabric's OpenAI endpoint instead of failing. A request that came through the fabric
  (`x-cf-hop`) is never passed on again; the answer carries `x-mm-offloaded` and `x-mm-node`. Off until `fabric.set` is
  used. See `docs/fabric.md`. Tests: `fabric::tests`, and six in `tests_upstream.rs` (busy hub offloads, hop guard, offload
  off, unreachable fabric, `fabric.set` checks, `machine.status`).
- An offloaded request is charged to the caller's key with the tokens in the fabric's answer (as one served here is, and
  counting against its quotas), and `fabric.status` gains `offloaded_by_node`. Tests: the offload test checks the count, and
  `an_offloaded_request_is_charged_to_the_callers_key_with_the_tokens_the_fabric_answered`.

- **The server log says how a model's layers were placed, and `model.telemetry` reads it** (`src/loadlog.rs`, `src/engine.rs`,
  `src/hub.rs`, `src/serving.rs`; [serving](docs/serving.md#the-server-log-and-model-telemetry)). llama.cpp b12726 prints its
  load details (devices, memory fit, layers on the GPU, every buffer, KV cache and state, compute graph and its splits, stage
  times) only at verbosity 4, so a stock llama-server is now started with `-lv 4` unless the owner's `extra_args` set a
  verbosity (about 19 KB per load, 2.8 KB per request; Rigel and Prism are unchanged). Each log opens with `# mm` lines:
  what the hub ran, with which environment, and why. The logs of the three loads before the current one are kept
  (`<model>.1.log` to `.3.log`), so the crash that caused a retry on the CPU can be read; `logs.tail` takes `previous`.
  New operation `model.telemetry`: the load report, the placement with layer counts, the latest request timings, the
  server's `/metrics` and `/slots`, and the GPU memory the process holds now. The placement label now follows the layer
  counts in the log: gemma-4-31B (every layer on the card) was labelled `partial` from a memory estimate and is now `gpu`,
  and its note no longer says that 19.9 GB of it stay in RAM (the estimate for a sliding-window model was 42 GB against
  14.5 GB measured). Measured through a debug hub with the b12726 engine on an RX 7900 XTX, 900-token replies:
  Huihui-Qwen3.8-27B-abliterated-UD-Q4_K_XL 37.8 tokens/s (66/66 layers, 16.6 GB, 52.6 s to load from an SSD),
  gemma-4-31B-it-UD-Q2_K_XL 41.2 tokens/s (61/61 layers, 14.5 GB, 109 s from a hard drive).

- **The VRAM estimate knows sliding-window attention** (`src/serving.rs`, `src/catalog.rs`; [serving](docs/serving.md#the-vram-planner)).
  The estimate took every layer for full attention with 32 KV heads of 512 (the query heads, because the GGUF reader
  skipped every list and gemma 4 keeps its KV heads per layer): gemma-4-31B came to 42,450 MiB for a load that took
  14,540 and was held back from the card for it. The reader now keeps short lists of numbers and flags (at most 1,024 entries), and
  the shape takes the heads, the head sizes (`key_length_swa`) and the window layers (`sliding_window_pattern`) per
  layer, plus layers that share the cache of earlier ones (`shared_kv_layers`). The window layers are sized as
  llama.cpp sizes them: the window and a batch, rounded to 256, for each slot (one window for all with `--kv-unified`),
  or the whole context with `--swa-full`. For gemma 2, gemma 3, gemma 3n, cohere2, gpt-oss, olmo2, afmoe and plamo3,
  whose files may lack the pattern, the period llama.cpp builds in is used (read from `engines/llama.cpp`); any other
  architecture with no pattern in its file stays full attention, which is the safe side. Checked against the real file:
  gemma-4-31B at 4,096 tokens now estimates 1,520 MiB of KV (the server printed 320 MiB for the 10 full layers and
  1,200 MiB for the 50 window layers) and 13,250 MiB in all, against 12,918 MiB projected by the server; Huihui
  Qwen3.8-27B (no window) is unchanged at 17,886 MiB. A 32k context on gemma now costs 3,760 MiB of KV, not 30 GB.

- **llama.cpp b12726 (Inkling 2), and a GPU crash gets a second try with the Vulkan cooperative-matrix kernels off**
  (`engines/llama.cpp`, `engines/patches/mm-routing.patch`, `src/engine.rs`). The pinned llama.cpp moved from 7fee17846 to
  the tag b12726, which runs `inkling2_loop`; the routing patch was re-based. A load that crashes llama-server on the GPU
  with 0xC0000409 is started once more on the GPU with `GGML_VK_DISABLE_COOPMAT` and `GGML_VK_DISABLE_COOPMAT2` set, and only
  then goes to the CPU; a model that works that way is remembered (`placements.json`: `safe_kernels`) and stays on the GPU.
  Measured on an RX 7900 XTX: Inkling-2-Mini-UD-Q8_K_XL 38.2 tokens/s on the GPU, 3.0 on the CPU it used to land on.

- **A model bigger than the memory can be loaded from disk, on purpose** (`src/serving.rs`, `src/engine.rs`; [serving](docs/serving.md)).
  New setting `allow_paged_load` (default off) and profile field `paged`: with both, a model that fits neither the GPU nor
  RAM is placed as `paged` (`model.plan` says `where: paged`) instead of refused, and `model.load` starts it with a
  30-minute minimum load deadline. A request, Misty and `auto` never load it (`409 needs_explicit_load`). With either
  switch off nothing changes. Rigel now lives in its own repository, `LoopyLuci/Rigel` (a standalone snapshot; the patch is gone). The Rigel build can pin an older MSVC toolset (`MM_RIGEL_MSVC_TOOLSET`), which the newest one needs.
  Rigel option `layer_readahead` (`--layer-readahead N`, off by default): reads the next layers of a memory-mapped dense model while the
  current one computes (3% cost on a model that fits, at most about 10% gain on a cold read; a test with most of the RAM held back
  was inconclusive because the model stayed cached; see docs/rigel.md).
  Rigel option `layer_resident_mib` (`--layer-resident-mib N`, off by default; -1 = what is free): locks the first layers of a dense model in memory
  and streams the rest (measured under a hard working-set cap: about 2 to 3 times the speed of none; noisy single runs).
  Rigel now uses its own `--fit` for the GPU/CPU split (it passed `-ngl 999`), and new setting `vram_extends_ram` (default on) puts layers on a
  card that has room when a load meant for the CPU does not fit the free RAM.
  A Prism (Bonsai) model wholly on the card is loaded with `-lm none`, so its weights are in VRAM only (RAM 0.45 GB against 2.2 GB).
  New setting `auto_rigel` (default on): a mixture-of-experts model too big for the card is served by Rigel with paged experts (22 GB model: 65.7 tokens/s in 19 GB of VRAM against 53.7 with llama.cpp's fit);
  a Rigel load wholly on the card gets `-lm none` (RAM 1.8 GB instead of 22.7 GB).
  AirLLM Phase 0 results: [airllm-spike-results](docs/airllm-spike-results.md).
- **MOSS-Image, MiniMax-H3-Image and LTX-2.5 work in ModelMistress, and the chat has a media canvas** (`src/image.rs`,
  `src/hub.rs`, `src/engine.rs`, `src/build.rs`, `frontend/src/chat/media.ts`, `frontend/src/components/MediaCanvas.vue`,
  `ModelViewer.vue`; [images](docs/images.md), [chat-canvas](docs/chat-canvas.md)).
  - *Engine.* `engines/stable-diffusion.cpp` is at master-1127-79dc7ca. The bundled build turns on WebP and WebM.
  - *Models.* Companion files are found by name: the MOSS tokenizer (`tokenizer.json`), a Qwen3.5 / Qwen3-VL / Gemma encoder
  and an audio VAE (`--tokenizer`, `--llm`, `--audio-vae`). MOSS-Image takes 50 steps, guidance 3, shift 5; MiniMax-H3-Image takes
  6 steps, guidance 1, euler, its own sigma schedule and `--rng cpu` (the request's own values win).
  Measured on the RX 7900 XTX: MOSS BF16 768 px, 30 steps, 232 s of sampling; MiniMax UD-Q4_K_XL 1024 px, 6 steps, 2.9 s a step.
  - *Vulkan.* `sd-cli` is started with `GGML_VK_DISABLE_COOPMAT=1` and `GGML_VK_DISABLE_COOPMAT2=1` (unless set): with the
  cooperative-matrix kernels it aborts without a message (0xC0000409) on the MOSS and Qwen3.5 graphs. `--offload-to-cpu` is no longer
  passed for these two models (a 1 GB compute buffer failed to allocate with it).
  - *Video.* `POST /v1/videos/generations` (and `frames`, `fps` on the image request) runs `sd-cli -M vid_gen` and returns a `.webm`
  with its sound; the file is served as `video/webm`.
  - *Canvas.* The chat has a Canvas pane: pictures, video, audio, 3D (GLB, glTF, OBJ, STL with three.js, loaded only when one is
  shown), drawings, PDF, sandboxed pages and text, taken from the messages (markdown pictures, links, bare URLs, fenced
  `svg` / `html`). Only http(s), data: and hub paths are accepted; pages run with scripts off. The page policy now allows `blob:` for
  pictures, media, fetches and frames (the canvas shows hub files from blob URLs; it was blocked before), and a file on another site
  is offered as a link, not loaded. LTX-2.x has sampling presets (distilled: 8 steps, guidance 1; dev: 20 steps, guidance 3).
  Measured LTX-2.5 distilled Q4_K_M, 25 frames at 512x320 with sound: 387 s of sampling; the Gemma 12B BF16 encoder (26 GB) took 19.7 minutes
  to encode the prompt on its first run, because it streams through the card in 50 segments.

### The tests

- **The tests no longer depend on the machine they run on** (`src/engine.rs`).
  - *Why nine tests failed.* `Engine::roots` added the folders found from the environment: `OLLAMA_MODELS`, the home
  folder's `.ollama` and the Hugging Face caches. On a machine with its own models (here `OLLAMA_MODELS` points at a
  store of models) those models were listed first, so tests that expect their own `tiny` model or no model got the
  host's instead.
  - *The fix.* A test build gets no machine folders; a test names the folders it needs in its settings. A test checks
  that a fresh engine's folders are all under its own home.

## 0.4.2 — 2026-11-23

- **A CPU model and a GPU model work at once without fighting for cores; loads show their progress; the starter set
  takes the GPU engine build** (`src/serving.rs`, `src/engine.rs`, `src/misty/setup.rs`; [serving](docs/serving.md)).
  - *Thread budget.* New setting `thread_budget` (default on). A server wholly on the card gets 2 to 4 threads (a third of
  the physical cores), a CPU-only server the cores the GPU servers leave, shared with the CPU servers running (`-t`).
  Split loads, a lone CPU model, small machines and any thread setting of the owner are left alone.
  - *Load progress.* `service.status` has `loading`: stage, percent, MiB read of the file, rate and time left for each
  server that is starting.
  - *Starter set.* `misty setup --install default` also installs `prism-gpu` when a GPU is found.

## 0.4.1 — 2026-11-23

- **Model loading holds to the memory the machine really has, and models say where they run**
  (`src/engine.rs`, `src/serving.rs`, `src/machine.rs`, `src/catalog.rs`, `src/misty/roster.rs`;
  [serving](docs/serving.md), [plan and findings](docs/HANDOVER.md)).
  - *Why GPU loads failed.* On Windows a GPU load is charged against the system's memory commit (about 1.2 times its
  size), so a load of 20 GB failed with `ErrorOutOfDeviceMemory` and 23 GB of VRAM free when the open programs had
  committed most of it, and worked later. The old retry changed nothing, and the CPU fallback was skipped for lack of RAM.
  - *The GPU keeps 2 GB for the system.* New setting `vram_reserve_mib` (default 2048): models use at most the card's
  total less that, 22 GB of a 24 GB card, counting the models already loaded.
  - *Loads are held, then split.* The first start is held to the smallest of the reserve, the commit headroom and what
  the card lately held; an out-of-memory failure is retried up to three times with the load held an eighth lower and the
  rest in RAM, then on the CPU. The limit the card showed is kept in `gpu_limits.json` (a failure for 24 hours). The final
  error names the commit when it explains the failure. `model.plan` adds `memory`.
  - *Labels.* Every model in `model.list` and `/v1/models` has `hardware` (engine, what it can run on, where it runs or
  would go, VRAM and RAM), `quant` and `label`; the Models page and `model-mistress models` show them.
  - *Misty* counts the card's room when asking whether a model fits, says where each role is meant to run, and keeps the
  embedder and reranker on the CPU so that the card is left to the language models.
  - *Ternary models on the GPU.* New setup component `prism-gpu` (the PrismML build with the Vulkan backend, 29 MB,
  pinned size and SHA-256; `model-mistress misty setup --install prism-gpu`; not in the starter set). Loads that put
  layers on the card use it, loads on the CPU use the CPU build. Measured on an RX 7900 XTX with the 2B `PQ2_0` file:
  prompt 3,205 against 211 tokens/s on the CPU, generation 100.7 against 46.8, the same answer. Misty keeps `fast`, the
  embedder and the reranker on the CPU so that a CPU and a GPU model work at once.
  - Not done yet (see the handover): load progress for slow disks, a thread budget for a CPU and a GPU model at the same
  time, a CPU draft model.

- **Misty chats have a working folder and environments, and Up and Down walk through earlier messages**
  (`src/misty/env.rs`, `frontend/`; [guide](docs/misty/USER_GUIDE.md), [API](docs/misty/API.md)).
  - *Environments.* A named, reusable setup the owner makes once: a working folder, other folders the file tools may
  reach, variables for `shell_run`, and a policy for each class of tool that replaces Misty's own for chats in it. New
  operations `misty.env.list`, `.save`, `.delete`, `misty.session.set` and `misty.browse` (only the owner's token reaches
  them, and none of her tools does, so she cannot widen her own folders or loosen her own policy); `env` and `cwd` on
  `misty.session.new`, `misty.ask` and `misty.run.start`; `env`, `env_name` and `cwd` on each listed session. Stored in
  the existing `state.db` (new `environments` table, two new columns on `sessions`; an old database is upgraded on open,
  and a session without either works exactly as before). The scope is read again at each tool call, so a change applies
  to a run that is going. Folders: the chat's own wins over the environment's, and with either one the file tools reach
  that folder and the environment's other folders only; the secret and escape guards hold inside all of them.
  - *Where it shows.* The Misty page has a bar above the chat (environment, working folder with where it comes from, what
  each class of tool does there) and a folder picker that lists folders on the machine the hub runs on; a new page
  **Misty, Environments** lists, makes, changes, duplicates and deletes them (turning commands or the network on without
  asking has to be confirmed). CLI: `misty ask|chat --env NAME --cwd FOLDER`, `misty sessions set ID --env NAME|none
  --cwd FOLDER|none`, `misty env`, `misty env delete NAME`.
  - *Message history.* In the message box of the Chat page and the Misty page, Up brings back the message sent before
  and Down walks forward to what was being typed (what was sent in other chats comes first, kept in the browser, the
  last 200). In a message of several lines the keys move the cursor and only the first line (Up) and the last (Down) walk
  the history. Ctrl+Enter now sends in the Misty box too.
  - *Plain Chat.* The Chat page's models have no tools, so a folder or environment has nothing to apply to there; they
  are for Misty chats.
  - *Found while checking in a browser, and fixed.* The folder picker waited for the dialog's `close` event to answer, and
  a page that is not being drawn (a background tab, a hidden pane) never gets it, so choosing a folder did nothing there;
  it now answers from its button. The folder browser showed `D:/work\docs` for a path written with `/` on Windows; it
  now uses one kind of slash.
  - *Checked.* Misty tests 125 (new: environment storage and upgrade, scope and policy in the agent loop with scripted
  models, the operations, the CLI, the browser), the whole library suite 559 passed, 6 ignored (`--test-threads=3`; with
  more threads the timing-sensitive factory tests fail on this machine from CPU contention, as before), clippy with
  warnings denied and `cargo fmt --check`; the dashboard `npm run check` (vue-tsc, 127 tests, build). In a browser against
  a scratch hub (debug build, scratch home): the environment bar, the picker (open, go into a folder, use it: the chat's
  folder changed on the hub), making an environment with a folder, a variable and a policy on the Environments page,
  **Use in a new Misty chat**, and Up and Down on both pages. Not run: the installed desktop app (it needs a new
  build), a real language model working in an environment (scripted models only), the CLI against a live hub (its
  tests only), and the folder picker on a hub on another machine.

- **The desktop app finds a restarted hub again.** A hub picks a new token every time it starts and writes it to
  `control.json`, often at the same address (the Scheduled Task uses a fixed port). The app kept the client it made
  at start for as long as `/v1/health` answered, and health needs no token, so after a restart every operation on the
  Models page (and every other page that calls an operation) failed with `unauthorized: this needs the hub's token or
  a peer key` until the app was closed and opened. It now compares the client's token and address with the control
  file on each call and makes a new client when they differ (`desktop-app/src/api.rs`; `Client::token()`). This
  matters more now that the watchdog restarts a hub that stops answering. Covered by a test that changes the token in
  the control file under a running link. Not run: the fix in the installed app (it needs a new build).

## 0.4.0 — 2026-11-22

- **The hub no longer wedges after a failed GPU load, restarts itself if it ever does, and places each model on the GPU or
  the CPU by what it can run on** (`src/engine.rs`, `src/serving.rs`, `src/watchdog.rs`,
  [docs/serving.md](docs/serving.md)).
  - *The wedge.* A llama-server that exited while loading (GPU out of memory, a driver crash, a bad file) deadlocked the
  thread that ran the load: the exit check held the lock on the child process in an `if let`, and `stop()` took the same
  lock. One stuck thread silences the whole async runtime, so `/v1/health` stopped answering while the operating system
  still accepted connections and the process sat at 0 % CPU, which the Scheduled Task cannot tell from a healthy hub.
  Reproduced with `nomic-embed-text` on the Vulkan GPU, read from symbolized thread stacks, fixed (the status is read
  before `stop()`), and covered by a test that runs the load on a runtime of its own with a timeout outside it.
  - *The watchdog.* `model-mistress serve` probes its own `/v1/health` every 10 s; after `watchdog_s` (default 90, 0 =
  off) without an answer it writes `<home>/logs/hang-<time>.txt` (when the runtime's timer last ran, memory and the stack
  of every thread on x86-64 Windows) and exits with code 70, so that the Scheduled Task starts a new hub. The
  desktop app's embedded hub does not run it.
  - *Placement.* With `gpu_layers` at its default (999) llama-server now chooses the layers itself (`--fit on
  --fit-target`, from what each device has free when the server starts) instead of being told `-ngl 999`: all on the GPU,
  part on the GPU and the rest from RAM, or on the CPU. The hub's header estimate decides which idle models to unload
  (a model on the CPU is never unloaded for VRAM) and refuses a load that fits neither (`503 insufficient_memory`).
  `model.plan` adds `placement`; a loaded model reports `serving.placement` (`gpu`, `partial`, `cpu`, `unknown`) and
  `serving.placement_note`; the Models page shows both and the split before a load. `gpu_layers = 0` now means no GPU
  backend at all (`-ngl 0 -dev none`).
  - *Fallback* (`gpu_fallback`, default on). A load that fails because of the GPU is tried again: out of memory first with
  2 GiB more kept free, then on the CPU; a driver crash goes straight to the CPU. A crash is remembered in
  `<home>/placements.json` against the llama-server program, so the next load starts on the CPU; `retry_gpu: true` (a box
  on the load form) tries the GPU again. Failures that are not the GPU's are never retried. `llama-tts` and `sd-cli` fall
  back to the CPU the same way. New settings `gpu_fallback` and `watchdog_s` are in the Settings page.
  - Run on this machine (Windows 10, RX 7900 XTX, amdvlk Vulkan driver, 2026-11-22), on a scratch hub: `nomic-embed-text`
  crashed the driver (exit 0xC0000409) and loaded on the CPU 5.8 s later while the hub kept answering; the next load went
  straight to the CPU in 5 s from the remembered crash, and `retry_gpu: true` tried the GPU again. `Bonsai-1.7B-Q2_K`
  placed itself on the GPU (878 MiB, `placement: gpu`); with the margin raised to leave 240 MiB on the GPU it reported
  `partial` and still answered; a request larger than the machine's RAM was refused up front. The new build then replaced
  the live hub's: health, `config.get`, `model.list`, `router.status`, `misty doctor` and an `auto` chat all worked.
  Checks: `cargo fmt`, `cargo clippy -D warnings`, 539 library tests pass (6 ignored), the dashboard type-checks and its
  81 tests pass.
  - Not run: the watchdog on a real hung hub (it has 7 tests against stand-ins, and ran 10 minutes without a false
  trigger); how long the Scheduled Task takes to start a hub after exit 70 (expected 1 to 5 minutes, not timed); a model
  larger than the GPU's memory; `--fit` with the Prism and Rigel engines (they keep `-ngl`); the new dashboard fields in a
  browser; crash detection from a signal on Linux and macOS (built for Windows only); the `llama-tts` and `sd-cli`
  fallback end to end (only its decision is tested).
- **Misty, a CPU-first agent** (`src/misty`, [docs/misty](docs/misty/USER_GUIDE.md)). One request goes through a router
  to a role (fast, main or deep, filled by your models; Ternary Bonsai 2 2B, 9B and 27B are preferred), with retrieval
  from her own index (keyword search, plus vectors when an embedding model is installed), tools under a class policy
  (read auto, write ask, exec deny, net deny, hub ask), one retry a role up when an answer looks weak, sessions, pipelines
  (TOML recipes; four built in) and Needle3, Whistle and Mirage through the Cactus runtime.
  - Surfaces: 29 `misty.*` operations (also MCP tools) with REST routes and an SSE run stream; the OpenAI models `misty`,
    `misty-fast`, `misty-main` and `misty-deep` (a peer key needs her named in its allow-list, and gets no tools, files or
    index); `model-mistress misty ...`; a Misty page and a setup page in the dashboard and desktop app; a Misty page in the
    Qt window.
  - New engine `engine = "prism"` runs ternary Bonsai files that the bundled llama.cpp (b11272) cannot; a ternary file with
    no engine able to run it is refused with `engine_cannot_run` (409) instead of loading and answering wrongly. Setting
    `prism_server` / `MM_PRISM_SERVER`; `misty setup` installs the engine, the Cactus runtime, the Cactus models and the
    Bonsai GGUFs (pinned sizes and SHA-256).
  - Her file tools reach only the folders you list; web and shell tools are off until you allow them; what a tool returns is
    marked as data; secret-looking files are never read or indexed.
  - Run on real artifacts (Windows 10, Ryzen 9 5900X, 2026-11-21): the Prism engine build 12128 with the `PQ2_0` Bonsai
    2B, 4B (speed only) and 9B, the Cactus runtime with Needle3, Whistle and Mirage, `misty doctor`, `misty bench`,
    retrieval with citations, tool calls, a spoken question, a picture question, the `voice-note` pipeline and the OpenAI
    route. That run found and fixed eleven defects: the Windows Cactus library (`libcactus.dll`) was not found; Needle was
    given its tools in an order that halved its accuracy; its reflex threshold (0.85) could never be reached (now 0.10,
    and only tools that read run on its word); every request was offered every tool, and the 9B reads about 30 tokens a
    second, so a file listing took 63 s (now only the tools a request is about, and the `fast` role words the result:
    10 s); the base prompt's "cite as [1]" made the small model write `[1]` with nothing retrieved; sources were listed
    that the answer never cited; `rag add` of a path that is not there reported success with zero files; Mirage's masks
    and boxes filled the prompt (picture question 50 s, now 26 s); picture input needed `--features vision` and its ONNX
    download (it now needs nothing); "the workspace" was taken as a folder name; a role override could not be removed
    (`config set` now follows JSON Merge Patch: `null` removes a key). Speeds are in the user guide. Not run: the
    Misty page of the dashboard and desktop app (type-checked, tested and built, not opened) and of the Qt window, other
    platforms' Cactus libraries, the `Q2_0` and `PTQ1_0` packings and the 27B file.
  - A twelfth defect, found when NLP-CLI was joined to her: the router did not cue arithmetic, so "what is 17 times 23"
    was offered no calculator and the 4B answered 411, 451 or 491 on three tries. `router::tool_hint` and the agent's
    tool focus now share `agent::is_arithmetic` (cue words, `%`, "square root", and number-operator-number sums,
    including "times", "x" and "over"); a sentence such as "how many times did we meet over 3 days" is not taken for a
    sum. Now 391 on three tries, and 36 for "15% of 240".
  - Found when her index was built over the docs of three projects (3,503 passages): an embedding failure was thrown away
    (`embed_pending` ended in `unwrap_or(0)`), so a model that rejected every call left `embedded: 0` with no reason.
    `rag.add` now reports `embed_error`, and a passage the embedder calls too large (longer than its batch) is cut in
    half and retried, up to four times. The bundled llama-server (0.5.0-dev) crashes (exit `0xC0000409`) on
    `nomic-embed-text` when it offloads to the Vulkan GPU, and on CPU with a batch above 512; a profile with
    `engine = "prism"`, `gpu_layers = 0` and larger `ctx_size`, `batch_size` and `ubatch_size` runs it (user guide).
- **The model router in every chat: the model `auto`**, and a Models page that manages models.
  - `auto` is a plain chat that goes to the model that suits each message. The hub picks the role (`fast`, `main` or
    `deep`) with Misty's router, forwards the chat to that role's model and says which answered in the headers
    `x-mm-routed-model`, `x-mm-routed-role` and `x-mm-routed-why`. It has no tools, files or index (that is `misty`).
    `@fast`, `@main` and `@deep` in a message choose the role and are removed before the model sees them. A
    conversation with a picture goes only to a model that has a projector. A peer key may use it only when its allow-list
    names `auto`, and the router then considers only models the key allows. With no model ready it answers 409
    `no_model_ready` with the reasons. A catalog model named `auto` wins. See [docs/misty/API.md](docs/misty/API.md).
  - Operations `router.status` (each role's model, whether it can run now, whether it is loaded, your overrides, free
    memory) and `router.pick` (which model a message or conversation would go to and why; nothing is sent).
  - Chat page (dashboard and desktop app) and the Qt window: Auto is the first choice and the default once a role has a
    model that can answer. A reply says which role and model answered and why (a "router" chip and a "why" list), and
    the strip above the chat shows the roles. The page asks `router.pick` first, so it can say which model is loading.
  - Models page: a **Router** tab (choose the model for each role, or Automatic; load or unload it; try a message and see
    where it would go and the scores), a role badge on each model with a "use for" menu, a per-model **Settings** editor
    for the serving profile (engine, GPU layers, context, parallel, batch sizes, flash attention, KV cache types, prompt
    cache, Jinja; validated by the hub, applied on the next load, "reset" removes it), a **fit check** in the Load form
    (`model.plan`: fits, fits after unloading what, or does not fit), a filter, a "show" menu and a sort, and embedding
    models in their own group. `model.list` now says `deletable` (ModelMistress downloaded it); Delete is off for the
    others instead of failing. The Qt window's Models page has the role choosers and a Router column.
  - Tests: 8 router tests and a hosting test in the hub, 75 Vitest tests (the Chat and Models pages are mounted against a
    stand-in hub), 14 new Qt and window-model tests. Not run: the pages against a real hub with a person at them (the
    dashboard asks for the owner token), and `auto` with a real picture model.
- **`raise_priority`** (Windows, off by default): a hub started at below-normal or idle priority, as a Scheduled Task is
  by default, lifts itself to normal, and the model servers it starts after that inherit it. Without it, CPU work in the
  hub (embedding, Bonsai on the Prism engine) ran four to five times slower than in a hub started by hand while other
  programs used the CPU. It never goes above normal and never lowers a priority. `config.set` takes
  `{"settings": {"raise_priority": true}}`, and the dashboard's Settings page has the switch. An older hub
  ignores the line, and drops it the next time it saves its settings.
- **Rigel, a second engine**: a model's serving profile can say `engine = "rigel"`. Rigel is a llama.cpp
  fork ported to Windows and AMD HIP, and it can page a MoE model's experts from disk into VRAM slots. The new profile
  options are `moe_direct_io`, `moe_slots`, `moe_pack_dir`, `moe_profile` and `moe_cache_mib`.
  - A Rigel model always gets one slot.
  - `backend.list`, `hub.info` and `model.plan` show the engine.
  - The new feature `rigel-engine` builds Rigel into the binary (from `engines/rigel` plus
    `engines/patches/rigel.patch`; GPUs from `MM_RIGEL_GPU_TARGETS`).
  - Paged decoding is slow until the VRAM slots warm up. This is still open; see the measurements in the doc.

  See [docs/rigel.md](docs/rigel.md).
- **Agents and Claude**: `model-mistress mcp --start` starts a hub in the background when none answers (it keeps
  running after the session), and `mcp --peer` serves only `models_list` and `chat_complete`, with a peer key from
  `MODELMISTRESS_KEY`. The MCP server now tells the client what its tools are for. A Claude Code plugin is in
  `integrations/claude/model-mistress` (slash commands, a skill, the MCP server), listed by `.claude-plugin/marketplace.json`.
  See [docs/claude.md](docs/claude.md), [docs/agents.md](docs/agents.md) and [docs/integrations.md](docs/integrations.md).
- **Finding the hub**: `MODELMISTRESS_URL` with `MODELMISTRESS_TOKEN` names a hub without a control file. Errors name
  the URL, the file and the variable that would fix them. An address from a control file that does not answer says the
  file may be a left-over copy, and names `MODELMISTRESS_URL` and `MM_HOME`. `--home` names the hub for that command, so `MODELMISTRESS_URL` does not
  override it. Hub errors reach MCP and CLI callers as `code: message`.
- **Peer keys**: `keys create` and `keys revoke` refuse to write `keys.json` while a hub runs in that home but does not
  answer. The running hub reads the file only at start, so such a write would not reach it (and a revoke would not stop
  its keys), and the command used to report success.
- **Key audit trail**: every peer key created or revoked is recorded in `keys.json` with its time, key, peer and who
  made it (`hub`, or `offline` for a command with no hub answering). `keys log` shows the record. A key change reads
  `keys.json` first, so a hub never undoes a change made by another process, and a damaged `keys.json` refuses key
  changes instead of being replaced. See [docs/hosting.md](docs/hosting.md).
- **Error codes**: chat, completions, embeddings and rerank answer llama-server's own errors under stable codes. A prompt
  over the context window is `context_exceeded` (400, with `n_prompt_tokens` and `n_ctx`); a server still loading is
  `loading`, and one that reports itself unavailable is `busy` (503, with `retry_after_s` and a `Retry-After` header).
  The `embeddings.create` and `rerank.run` operations and the Ollama shim's chat, generate and embed routes answer the
  same codes; the shim's `{"error"}` body gains a `code` field. See [docs/agents.md](docs/agents.md).
- **Fail-fast `busy`** (`fail_fast_busy`, off by default): with it on, a request that finds no idle slot in a model's
  llama-server gets `busy` (503, `Retry-After: 1`) at once, instead of waiting in the server's queue. The slot is checked
  with llama-server's `GET /slots?fail_on_no_slot=1` before each request on `/v1`. The dashboard's Settings page has the
  switch; `config.set` takes `{"settings": {"fail_fast_busy": true}}`.
- **Fix**: a hub started from `mcp --start` no longer keeps the MCP client's stdio pipes open (Windows passed them on).
- **A Python desktop window** (`gui/`, PySide6 6.12.0): Status, Models, Chat and Settings. It is a client of the hub's
  HTTP API (standard library only on the Python side), so the hub keeps no Python dependency. Run `python -m mm_gui`
  from `gui/`; see [gui/README.md](gui/README.md).
- **Window look**: Settings has an Appearance group (theme: follow the system, light or dark; style, Fusion by default;
  text size), kept per user and never sent to the hub. Ctrl+1 to Ctrl+4 switch pages, Ctrl+Enter sends in Chat, and the
  window remembers its size. [docs/gap-analysis.md](docs/gap-analysis.md) lists what is still missing in the hub,
  the dashboard and the window.
- **Dashboard gates**: `npm run check` in `frontend/` runs a vue-tsc type-check, Vitest unit tests (26) and the build.
  `ci/pipeline.py` has a `dashboard` stage (the same checks, from the lock file) and a `gui` stage (the window's
  tests, with the pinned PySide6 and pytest). `src/build.rs` rebuilds the hub when `frontend/dist` changes.
- **`MM_HOME` is the home folder everywhere**: `default_home()` (used by every client that finds the hub without a
  folder), the desktop app and the CI pipeline now use `MM_HOME` when it is set (blank values are ignored), else the
  platform folder as before. `--home` still wins for one command. The CLI already read `MM_HOME`; it now goes
  through the same rule.

- **`ollama_discovery` setting** (default `true`, so nothing changes): `false` stops using Ollama models entirely.
  No Ollama store is scanned (the detected ones, `ollama_dirs`, the download folder's `ollama` store), and
  `ollama/<name>` is refused with an error, and `model.pull` from the Ollama library is refused too. `backend.list`
  reports Ollama as turned off by config. Also in the desktop settings form.

## 0.3.0 — 2026-10-15

ModelMistress serves coding agents, and the native-model system gains experts, specialists and routing traces.

### Serving for agents
- **Per-model serving profiles**: context per slot, KV cache type, flash attention, prompt cache and reuse, Jinja
  tool-call templates and batch sizes, set per model in `[profiles]`.
- **VRAM planner**: estimates what a model needs before loading it and refuses, or evicts idle models, instead of
  spilling into shared memory (on by default). Serving details are reported on `/v1/models`.
- A typo in `[serving]` or `[profiles]` is a warning (logged, and listed as `config_warnings` in
  `service.status` and `config.get`); that field falls back to its default instead of stopping the hub.
- Speculative decoding now really uses its draft model (`--spec-type`). Measured on a 24 GB GPU it was slower for
  Qwen3-Coder-30B-A3B at 8k and 32k, so it stays off unless set per model.

### Native models and Knowledge Modules
- **Experts KMs** are assembled into their core model and hot-swapped in.
- **Specialist KMs are callable** (`km.specialist.call`): the input is checked, the engine runs the KM's model, and
  the call is recorded.
- **Expert-routing capture**: a llama-server patch reports which experts each MoE layer chose per token, and traces
  record it (no cost when off; about 4.5x slower generation while on).

### Fixes
- Tests delete their own temp folders (about 2,000 had piled up).
- Engine patches are stored byte for byte, so a CRLF checkout no longer makes `git apply` refuse them.

## 0.2.1 — 2026-10-11

The desktop app, ready for daily use.

### Desktop app
- **An installable Windows app** (NSIS per-user installer, and an MSI). It ships the hub beside it, starts it when
  the app opens (or uses a hub that already runs) and stops it on quit only if the app started it
  (`desktop.json` `{"stop_hub_on_quit": false}` or `MM_STOP_HUB_ON_QUIT=0` keeps it running). Tray: Open, Stop hub,
  Quit. A starting screen, and an error screen with Try again.
- **Chat**, the landing page: streaming replies with markdown, collapsible reasoning, Stop (or Esc), conversations,
  and a link from a reply to its trace.
- **Models**: load and unload with options, including experts on the CPU (`--cpu-moe`, `--n-cpu-moe`); pull by name;
  ModelMistress's own native models are listed (a GGUF with its `dusty.json` beside it), with experts on the CPU by
  default.
- **Status** (RAM, the loaded model's memory and buffers), **Settings**, **Speech**, **Images** and **Vision** pages.
  The old Swarm page is gone.
- Moved to Tauri 2. The window reaches the hub through a loopback relay that adds CORS headers for the app's own
  origins only (it adds no credentials). The content security policy allows only `http://127.0.0.1`.

### Hub
- `service.status` reports system RAM; a loaded model reports its resident memory and buffers.

### Also in this release
- **The Forge** (`docs/native/forge.md`): an optional Python/PyTorch environment the hub runs; Dusty-1.0 (Qwen3.5-4B
  as `qwen35moe`) converts and answers token for token like its base.
- **The Knowledge Module factory** (`docs/native/km-factory.md`): tasks from plan to publish, with review.

## 0.2.0 — 2026-10-10

The first release of ModelMistress as a standalone model hub.

### Models, built in
- **llama.cpp built into the binary** (`--features bundled-engine`): `llama-server`, `llama-quantize`, `llama-tts`
  and stable-diffusion.cpp's `sd-cli`, Vulkan by default; nothing else to install but the GPU driver.
- **Downloads** from Hugging Face (`hf.co/<org>/<repo>:<quant>`) and the Ollama library, with progress, resume and
  SHA-256 checks.
- **Speech to text** (`--features whisper`): OpenAI's `/v1/audio/transcriptions` and `/translations` through
  whisper.cpp; `transcribe`.
- **Text to speech:** `/v1/audio/speech` and `speak` through llama.cpp's `llama-tts` (for example Qwen3-TTS).
- **Image generation:** `/v1/images/generations`, the `image.generate` job and `image` (SD 1.x/2.x, SDXL, FLUX).
- **Image recognition** (`--features vision`): classify, detect and embed with ONNX models through ONNX Runtime;
  preprocessing matches the reference implementations (letterboxed detectors, crop_pct, antialiased resize).
- **llama.cpp's tools as operations:** quantize, LoRA adapters, a draft model for speculative decoding, embeddings
  and reranking beside a chat model.

### Knowledge Modules and the factory
- **Knowledge Modules (`mmkm/1`):** versioned, tagged, optionally signed packages of knowledge, skills and tools that
  work beside a model's weights; safe packing, hash checks on every load, explained search, disk/RAM/VRAM residency
  budgets, training data with revision history, a controlled tag vocabulary.
- **The factory:** tasks that plan, gather, generate cited records, verify them with a second model, tag them, build
  and evaluate KMs, and publish them under a promotion policy; resumable, budgeted and traced.

### Observability and the dashboard
- **Traces of the model's thoughts:** tokens with probabilities, alternatives and entropy, reasoning apart from the
  answer, stage timings and resources; rerun with changes and compare.
- **A dashboard at `/ui`** in any browser and in the desktop app: Thoughts, Traces, Knowledge, residency, Data studio,
  Vocabulary, Factory.

### Hosting and control
- **Peers** use this hub's models with their own keys, quotas, allow-lists and usage records; the owner's token stays
  local.
- **Control API and MCP:** every operation has a JSON Schema and is also an MCP tool (`model-mistress mcp`).
- **The desktop app** runs on the hub (no Ollama needed).

### Designed, not in this release
- MM's own models (Dusty and Misty), the training system and consolidation: `docs/native/architecture.md`.
