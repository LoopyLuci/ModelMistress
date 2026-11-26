# Desktop parity with the Android app

Checked 2026-11-26. The Android list is `android/docs/FEATURE_MAP.md`. The desktop side is the Tauri app
(`desktop-app/`), the web dashboard (`frontend/src`), the hub (`src/`) and the native PySide window (`gui/mm_gui`). The
owner asked for the web dashboard and the native window to both carry the Android features, so both are counted here.

Status: **Present**: on the desktop already. **Added**: built, with tests. **Partly**: some of it.
**Missing**: not on the desktop. **Android only**: an Android runtime or phone feature with no desktop counterpart.
**Not checked**: not verified in this pass. Where a row says "native", the feature is in the PySide window too.

## Hub and API

| Android feature | Desktop | Where | Status |
| --- | --- | --- | --- |
| OpenAI `/v1` API: health, models | The hub's `/v1` routes | `src/hub.rs` | Present |
| `/v1/chat/completions`, non-streaming and streaming | Same route, streamed to the dashboard and the native window | `src/hub.rs`, `src/client.rs` | Present |
| Bearer API key auth | The hub checks `Authorization: Bearer` | `src/hub.rs` (auth check) | Present |
| CORS | Browser pages call the desktop hub | — | Not a gap (Android deliberately omits it) |
| `auto` model routing | The chat's router and Misty's router | `frontend/src/chat/router.ts`, `src/misty/router.rs`, `gui/mm_gui/chat_page.py` | Present |
| Several models served at once | The hub routes each request by model name | `src/engine.rs` (`body.model`) | Present |
| Image generation: `POST /v1/images/generations` | Route wired; Images page | `src/hub.rs` (route), `src/image.rs`, `frontend/src/views/ImagesView.vue` | Present |
| Speech to text and text to speech | `/v1/audio/transcriptions`, `/v1/audio/speech`; the Speech page and Dictate in the chat | `src/audio.rs`, `src/tts.rs` | Present |
| Transferd transport to the desktop (publisher side) | The overlay publisher and its loopback test | `overlay/src/lib.rs`, `overlay/tests/loopback.rs` | Present (no dashboard screen) |
| Chat template from the GGUF file; GGUF metadata reading | Engine detail | — | Not checked |
| Embeddings (Android: Next) | Not checked on the desktop | — | Not checked |

## Models

| Android feature | Desktop | Where | Status |
| --- | --- | --- | --- |
| Hugging Face downloads, with resume and SHA-256 | Hugging Face pull and the Models page | `src/pull.rs`, `frontend/src/views/ModelsView.vue` | Present (resume and SHA-256 not checked) |
| Model list, default model, delete | Models page; the native Models page has the list and the router roles | `frontend/src/views/ModelsView.vue`, `gui/mm_gui/pages.py` | Present |
| Importing a GGUF file from the device | "Import model file" copies a `.gguf` into the models folder, with progress (web and native) | `frontend/src/modelImport.ts`, `ModelsView.vue`; `gui/mm_gui/pages.py`; `src/model_import.rs` | Added (not run on a large real file) |
| Device benchmark: stress test, memory copy, timed runs | "Run the speed test" on the Models page; the native Models page has a Speed test group | `frontend/src/views/ModelsView.vue`, `src/benchmark.rs` (`system.bench`), `gui/mm_gui/pages.py` | Added |
| Model suggestions | Fastest, balanced (keeps up with reading) and best (fits in memory), from the speed test | `src/benchmark.rs` (`suggest`), `ModelsView.vue`, `gui/mm_gui/pages.py` | Added |
| LiteRT-LM chat models (`.litertlm`) | Desktop runs GGUF through its own engine | — | Android only |

## Chat

| Android feature | Desktop | Where | Status |
| --- | --- | --- | --- |
| Chat screen, streaming, history | Chat page; conversations kept in the browser (web), and in memory (native) | `frontend/src/views/ChatView.vue`, `frontend/src/chat/store.ts`, `gui/mm_gui/chat_page.py` | Present |
| Sampling: temperature, top-p, max tokens | The chat's settings | `ChatView.vue` (temperature, top-p, max tokens) | Present |
| Sampling: top-k and seed | The chat's settings: Top-k (0 is off) and Seed (empty or -1 is random); the native window has both boxes | `ChatView.vue`, `gui/mm_gui/chat_page.py` | Added |
| Pictures attached to a message | Picture button, for vision models (native: Picture...) | `ChatView.vue` (`accept="image/*"`), `gui/mm_gui/chat_page.py` | Present |
| Text and PDF documents attached | Attach text, source, JSON, CSV and PDF files; a PDF's text comes from the hub, and each file's text is cut to 2000 characters | `ChatView.vue` (`docs`), `frontend/src/chat/documents.ts`; `gui/mm_gui/attachments.py` | Added |
| Camera: photo, and camera watched in Live | A camera button for photos (web and native); each Live sentence goes with the picture the camera has then | `frontend/src/chat/camera.ts`, `ChatView.vue`; `gui/mm_gui/audio.py` (`Camera`), `gui/mm_gui/live.py` | Added (not run with a real camera) |
| Speech input (mic into the message box) | "Dictate" in the chat, and the Speech page; the native window has Dictate | `frontend/src/speech.ts`, `ChatView.vue`, `SpeechView.vue`, `gui/mm_gui/audio.py` | Added (chat); Present (Speech page, fixed, see below) |
| Read aloud of each reply | "Read aloud" under each reply; the native window has "Read replies aloud", "Read last reply" and "Stop speaking" | `frontend/src/components/MessageView.vue`, `speech.ts`, `gui/mm_gui/chat_page.py` | Added |
| Chat auto-scroll; Jump to latest | "Jump to latest" when scrolled up | `ChatView.vue` (`behind`, `jumpToLatest`) | Added (web) |
| Steer during a reply (a note, without stopping it) | "Steer": the reply is cut after its next sentence, the text so far is kept, and the note goes in next | `ChatView.vue` (`steerNow`), `frontend/src/chat/extras.ts` (`steerCut`), `gui/mm_gui/chat_page.py`, `gui/mm_gui/chat_extras.py` | Added (both). A cut reply is not rated |
| Queue a message to send after the reply | A message written during a reply waits and sends next (chat and Misty page) | `ChatView.vue` (`queue`, `drainQueue`), `MistyView.vue`; `gui/mm_gui/chat_page.py`, `gui/mm_gui/misty_page.py` | Added (both) |
| Edit before send | Dictated text lands in the message box and can be edited before Send; in Live, "Edit before send" holds each sentence for editing | `ChatView.vue`; `frontend/src/live/turns.ts`, `gui/mm_gui/turns.py` | Added |
| Live conversation: continuous listening, echo matching, hold before send, Edit before send mode, interrupt, voice corrections | The Live button in the chat: the owner talks, each sentence is written down by the hub and sent, the reply is read aloud a sentence at a time, and a spoken correction is saved for Misty | `frontend/src/live/` (`LiveOverlay.vue`, `turns.ts`, `cutter.ts`, `segmenter.ts`); `gui/mm_gui/live.py`, `turns.py`, `cutter.py`, `segmenter.py` | Added (both). See the caveats under Open items |
| Sandboxes per chat, linked both ways | Environments and working folders; no links between environments (`src/misty/env.rs` has no link concept) | `src/misty/env.rs`, `frontend/src/views/EnvironmentsView.vue` | Partly |

## Misty

| Android feature | Desktop | Where | Status |
| --- | --- | --- | --- |
| Misty agent: routing, tools, Needle first | Misty page with the same roles and tool classes | `frontend/src/views/MistyView.vue`, `gui/mm_gui/misty_page.py`, `src/misty/` | Present |
| Cactus Needle as the reflex for an obvious tool | Reflex in the router | `src/misty/cactus.rs`, `src/misty/agent.rs` | Present |
| Tools: clock, calculator, read-only files (`fs_list`, `fs_read`, `fs_search`) | `datetime` (the clock), `calc`, and the three read tools | `src/misty/toolbox.rs` | Present |
| Tool use and file-write settings (Off, On, Full auto) | Policy per tool class: Auto, Ask, Deny (read, write, exec, net, hub) | `src/misty/config.rs` | Present (a different shape) |
| Ratings of replies: Good, Bad, No tool needed, Should have used a tool | A row under each Misty reply (native: a reply picker and rating buttons) | `MistyView.vue`, `gui/mm_gui/misty_page.py`, `src/misty/feedback.rs`, `misty.rate` | Added (both) |
| Corrections, given to Misty as a note for similar questions | "Correct it..." under a reply; the two most similar corrections go to the model (`NOTE_LIMIT = 2`) | `src/misty/feedback.rs` (`correction_note`), `src/misty/agent.rs` | Added |
| Routing taught by ratings | A "No tool needed" or "Should have used a tool" verdict sets the tools offered for similar questions. Corrections do not route, as on Android | `feedback.rs` (`taught`), `agent.rs` (`apply_taught`) | Added |
| Training export (chat-format JSON lines) | Learning tab on the Misty setup and status page; "What she learns..." in the native window | `frontend/src/views/MistySystemView.vue`, `misty.training.export`, `gui/mm_gui/learn.py` | Added (both) |
| Forget the ratings (one, or all) | With a confirmation | `MistySystemView.vue`, `misty.feedback.forget`, `gui/mm_gui/learn.py` | Added (both) |
| Queue during a reply, on the Misty page | A message sent during a reply waits and goes next | `frontend/src/views/MistyView.vue` (`MistyView.queue.test.ts`), `gui/mm_gui/misty_page.py` | Added (both) |

## What was broken on the desktop, and is fixed

- **Dictation on the Speech page did not work.** The page recorded WebM, and the hub reads only WAV and MP3
  (`src/audio.rs`). Recordings and uploaded files are now written out as WAV in the browser first
  (`frontend/src/wav.ts`), so the Speech page and the chat's Dictate share one path. A unit test covers the WAV writer.
  Not yet run with a real microphone or a real recording.

## Android only (no desktop counterpart planned)

- Foreground service for the server; headless router build; paired desktop hub (the desktop is the hub); the Transferd
  phone link (the desktop publisher exists, see above); LiteRT-LM chat models.
- Desktop-only features the phone does not have: Ollama `/api`, speech (whisper), vision (ONNX), Knowledge Modules,
  traces, MCP over stdio, the `/ui` dashboard. These are not parity gaps.

## Verification

- Rust: `cargo test` passed on every target: 649 library tests, 2 binary tests and 16 desktop-app tests, with 6 ignored
  and none failing. `cargo test --lib misty` (145 tests) covers the ratings, routing, correction notes, export, forgetting
  and the `misty.route` change.
- Frontend: `vitest run` passed (25 files, 217 tests), `vue-tsc --noEmit` is clean and `vite build` passed.
- Native window: `python -m pytest tests` in `gui/` passed 144 tests offscreen. With `MM_HUB_EXE` set to the debug hub
  binary, `tests/test_real_hub.py` also passed, which makes 145. The Live rules are ports of the web's tests
  (`turns.test.ts`), and the Live panel, the chat page, Misty's queue, steering and ratings, the learn dialog, attachments,
  the model import and the speed test each have offscreen tests with fake sound and a fake hub.
- Not run: the Tauri window; the native window and the web dashboard on a real microphone, camera or speaker; a real
  GGUF import of a large file; a real speed test on this machine.

## Open items

1. **Echo.** The native window has no echo cancellation, so the computer's own voice can reach the microphone. The
   echo rules (`turns.py`, ported from `turns.ts`) take out what sounds like the reply, but on a loud speaker a headset is
   the reliable setup. Owner decision: accept this and recommend a headset, or add echo cancellation (a larger change).
2. **Steer and ratings.** A reply cut by a steering note has no rating, on both platforms, because the text is not the
   whole reply. Owner decision: keep that, or rate the cut text.
3. **Live interrupt.** The native window can interrupt a reply early; the web Live cannot yet.
4. **Real hardware.** The microphone, camera, speaker playback, a large GGUF import and the speed test on this machine
   have not been run. These need a manual pass before the features are called verified on a device.
5. **The native window.** The owner asked for a full native window as well as the web one, so it is counted in this
   audit and built to the same feature list.
