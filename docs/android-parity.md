# Desktop parity with the Android app

Checked 2026-11-26. The Android list is `android/docs/FEATURE_MAP.md`. The desktop side is the Tauri app
(`desktop-app/`), the dashboard (`frontend/src`) and the hub (`src/`). The PySide window (`gui/mm_gui`) is not covered:
the owner has not said whether it counts as "the desktop app".

Status: **Present**: on the desktop already. **Added**: built in this change, with tests. **Partly**: some of it.
**Missing**: not on the desktop. **Android only**: an Android runtime or phone feature with no desktop counterpart.
**Not checked**: not verified in this pass.

## Hub and API

| Android feature | Desktop | Where | Status |
| --- | --- | --- | --- |
| OpenAI `/v1` API: health, models | The hub's `/v1` routes | `src/hub.rs` | Present |
| `/v1/chat/completions`, non-streaming and streaming | Same route, streamed to the dashboard | `src/hub.rs`, `src/client.rs` | Present |
| Bearer API key auth | The hub checks `Authorization: Bearer` | `src/hub.rs` (auth check) | Present |
| CORS | Browser pages call the desktop hub | — | Not a gap (Android deliberately omits it) |
| `auto` model routing | The chat's router and Misty's router | `frontend/src/chat/router.ts`, `src/misty/router.rs` | Present |
| Several models served at once | The hub routes each request by model name | `src/engine.rs` (`body.model`) | Present |
| Image generation: `POST /v1/images/generations` | Route wired; Images page | `src/hub.rs` (route), `src/image.rs`, `frontend/src/views/ImagesView.vue` | Present |
| Speech to text and text to speech | `/v1/audio/transcriptions`, `/v1/audio/speech` | `src/audio.rs`, `src/tts.rs` | Present |
| Transferd transport to the desktop (publisher side) | The overlay publisher and its loopback test | `overlay/src/lib.rs`, `overlay/tests/loopback.rs` | Present (no dashboard screen) |
| Chat template from the GGUF file; GGUF metadata reading | Engine detail | — | Not checked |
| Embeddings (Android: Next) | Not checked on the desktop | — | Not checked |

## Models

| Android feature | Desktop | Where | Status |
| --- | --- | --- | --- |
| Hugging Face downloads, with resume and SHA-256 | Hugging Face pull and the Models page | `src/pull.rs`, `frontend/src/views/ModelsView.vue` | Present (resume and SHA-256 not checked) |
| Model list, default model, delete | Models page | `frontend/src/views/ModelsView.vue` | Present |
| Importing a GGUF file from the device | The hub loads a `.gguf` by path or from the models folder; the Models page has no file picker (`ModelsView.vue` has no import control) | `src/hub.rs` | Partly (see the owner question below) |
| Device benchmark: stress test, memory copy, timed runs | Misty's "Time each model" times the ready roles only | `src/misty/ops.rs` (`misty.bench`), `MistySystemView.vue` | Partly |
| Model suggestions | No suggestion code found in `frontend/src` | — | Missing |
| LiteRT-LM chat models (`.litertlm`) | Desktop runs GGUF through its own engine | — | Android only |

## Chat

| Android feature | Desktop | Where | Status |
| --- | --- | --- | --- |
| Chat screen, streaming, history | Chat page; conversations kept in the browser | `frontend/src/views/ChatView.vue`, `frontend/src/chat/store.ts` | Present |
| Sampling: temperature, top-p, max tokens | The chat's settings | `ChatView.vue` (temperature, top-p, max tokens) | Present |
| Sampling: top-k and seed | Not in the chat (top-k appears only in the Traces and Knowledge pages) | `ChatView.vue` | Missing |
| Pictures attached to a message | Picture button, for vision models | `ChatView.vue` (`accept="image/*"`) | Present |
| Text and PDF documents attached | Not in the chat | `ChatView.vue` | Missing |
| Camera: photo, and camera watched in Live | Not built (the only capture is the microphone) | — | Missing |
| Speech input (mic into the message box) | "Dictate" in the chat, and the Speech page | `frontend/src/speech.ts`, `ChatView.vue`, `SpeechView.vue` | Added (chat); Present (Speech page, fixed, see below) |
| Read aloud of each reply | "Read aloud" under each reply | `frontend/src/components/MessageView.vue`, `speech.ts` | Added |
| Chat auto-scroll; Jump to latest | "Jump to latest" when scrolled up | `ChatView.vue` (`behind`, `jumpToLatest`) | Added |
| Steer during a reply (a note, without stopping it) | Not built; needs the agent to cut a reply at a sentence end | — | Missing |
| Queue a message to send after the reply | Chat: a queued message waits and sends next. Misty page: not built | `ChatView.vue` (`queue`, `drainQueue`) | Added (chat); Missing (Misty page) |
| Edit before send | Dictated text lands in the message box and can be edited before Send | `ChatView.vue` | Partly (the Live hold is not built) |
| Live conversation: continuous listening, echo matching, hold before send, Edit before send mode, interrupt, voice corrections | Not built. Dictation and read-aloud exist; there is no continuous listening, echo matching, hold, or voice correction | — | Missing (large; needs a design choice) |
| Sandboxes per chat, linked both ways | Environments and working folders; no links between environments (`src/misty/env.rs` has no link concept) | `src/misty/env.rs`, `frontend/src/views/EnvironmentsView.vue` | Partly |

## Misty

| Android feature | Desktop | Where | Status |
| --- | --- | --- | --- |
| Misty agent: routing, tools, Needle first | Misty page with the same roles and tool classes | `frontend/src/views/MistyView.vue`, `src/misty/` | Present |
| Cactus Needle as the reflex for an obvious tool | Reflex in the router | `src/misty/cactus.rs`, `src/misty/agent.rs` | Present |
| Tools: clock, calculator, read-only files (`fs_list`, `fs_read`, `fs_search`) | `datetime` (the clock), `calc`, and the three read tools | `src/misty/toolbox.rs` | Present |
| Tool use and file-write settings (Off, On, Full auto) | Policy per tool class: Auto, Ask, Deny (read, write, exec, net, hub) | `src/misty/config.rs` | Present (a different shape) |
| Ratings of replies: Good, Bad, No tool needed, Should have used a tool | A row under each Misty reply | `MistyView.vue`, `src/misty/feedback.rs`, `misty.rate` | Added |
| Corrections, given to Misty as a note for similar questions | "Correct it..." under a reply; the two most similar corrections go to the model (`NOTE_LIMIT = 2`) | `src/misty/feedback.rs` (`correction_note`), `src/misty/agent.rs` | Added |
| Routing taught by ratings | A "No tool needed" or "Should have used a tool" verdict sets the tools offered for similar questions. Corrections do not route, as on Android | `feedback.rs` (`taught`), `agent.rs` (`apply_taught`) | Added |
| Training export (chat-format JSON lines) | Learning tab on the Misty setup and status page | `frontend/src/views/MistySystemView.vue`, `misty.training.export` | Added |
| Forget the ratings (one, or all) | With a confirmation | `MistySystemView.vue`, `misty.feedback.forget` | Added |
| Queue during a reply, on the Misty page (Android's Misty chat is the same chat screen) | Not built on the Misty page; the Chat page has it | `frontend/src/views/MistyView.vue` (`send()` refuses while busy) | Missing |

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

- Rust: `cargo test --lib misty` passed (145 tests), including the new tests for ratings, routing, correction notes,
  export, forgetting, and the `misty.route` change.
- Frontend: `vitest` for `misty.test.ts`, `wav.test.ts`, `MistyView.test.ts` (rating tests), `MistySystemView.test.ts`
  and `ChatView.features.test.ts`; `vue-tsc --noEmit` and `vite build` passed.
- Not run: the desktop app itself. None of the rating, queue, dictation, or read-aloud paths were used in the Tauri
  window or with a real microphone or speaker.

## Left for the owner to decide

1. Live conversation on the desktop: the largest Android feature. It needs a design for continuous listening in the
   browser or the Tauri window, and whether the echo and hold rules carry over.
2. Steer during a reply: needs the agent to cut a reply at a sentence end.
3. Camera input (photo, and the camera watched during Live).
4. Text and PDF attachments in the chat (Android reads them; the desktop accepts pictures only).
5. Top-k and seed in the chat's settings (small; not built here).
6. A GGUF import control on the Models page (the hub already loads a `.gguf` by path).
7. Device benchmark and model suggestions (only "Time each model" exists).
8. Whether the PySide window counts as "the desktop app" for this audit.
