# Media plan: seeing, hearing and making (computer vision, video, meetings, video and 3D generation)

Status: **plan plus measured spike results, nothing built in MM yet** (written 2026-11-26, spikes run 2026-11-27). Every claim is
tagged: **[repo]** read in this checkout, **[web]** reported by a source listed in section 10 and not re-measured here,
**[spike]** measured on the development machine (section 6), **[call]** a judgement. Section 6 holds the numbers that decided
the design; where an earlier statement was wrong or incomplete, the text says so.

## 1. What is wanted

1. **See.** OpenCV-class computer vision inside ModelMistress (MM), and an assistant that can look at what is on the screen.
2. **Watch.** Take in videos, livestreams and meetings: transcribe them, understand what is shown, take notes, answer
   questions, act as an assistant while it happens.
3. **Make.** A local replacement for Higgsfield-style video generation (the plan the owner began with the ABP agent), and
   **3D asset** generation, built into MM properly rather than as a side project.

## 2. What exists today (build on it, do not duplicate it)

| Capability | State | Gap |
|---|---|---|
| Speech to text | **[repo]** whisper.cpp through `whisper-rs` (feature `whisper`), `/v1/audio/transcriptions`, `speech.transcribe`; segment timestamps; json, text, srt, vtt. CPU build. | Only WAV and MP3 are read (`src/audio.rs`); no word timestamps, no speaker labels, no streaming, no GPU. |
| Image recognition | **[repo]** ONNX Runtime (`ort`, feature `vision`): classify, detect (YOLO), embed (CLIP). CPU. PNG and JPEG only. | No video, no OCR, no tracking, no segmentation. |
| Vision chat | **[repo]** `llama-server` with an `mmproj`. The pinned llama.cpp (**b12726**) takes **image, audio and video** input through libmtmd; its server accepts `input_video` / `video_url` content parts; video is decoded by an **`ffmpeg` binary on PATH** (`MTMD_VIDEO`: on by default except on mobile and WASM, and `src/build.rs` does not change it). | MM ships no ffmpeg, so video input cannot work today; whether the hub passes such parts through is not tested. **[spike]** With a pinned ffmpeg given by `--video-ffmpeg-dir` the bundled server answers questions about a video (section 6, S2); a local file must be passed as `file://name` under `--media-path` (or as base64 or a URL), a bare path is read as base64 and fails with HTTP 500. |
| Audio-capable chat models | **[repo]** llama.cpp's own list: Gemma 4 E2B/E4B (audio + vision), Qwen3-Omni, Qwen2.5-Omni, Qwen3-ASR 0.6B/1.7B, Voxtral, Ultravox, SmolVLM2 (video-tuned). | Not wired to any MM operation. |
| Image and video generation | **[repo]** `sd-cli` (stable-diffusion.cpp **master-1127**), one process per request. Video: Wan 2.1/2.2, LTX-2.3/2.5, HunyuanVideo 1.5, MiniMax-H3, LingBot-Video. Video upscale: SeedVR2, RelayVSR; ESRGAN for images. `POST /v1/videos/generations`, `image.generate` with `frames`. | No queue of shots, no post tools, no asset library, no camera or consistency controls. |
| 3D viewing | **[repo]** the chat canvas shows GLB, glTF, OBJ, STL (three.js). | Nothing makes a 3D asset. |
| GPU Python worker | **[repo]** the Forge: a managed Python environment with **PyTorch ROCm for Windows** (torch 2.9.1, ROCm 7.2.1) whose preflight passed on the RX 7900 XTX. | Used for training only. |
| Cache and memory | **[repo]** CacheIt (opt-in cache for embeddings and pages), Misty agent with a toolbox, `/v1/embeddings`. | Nothing indexes media. |
| Measured generation speed | **[repo]** LTX-2.5 distilled Q4_K_M, 25 frames at 512x320 with sound: **387 s** of sampling (first prompt encode 19.7 min because the 12B encoder streams through the card). Image models: MOSS 768 px 30 steps 232 s; MiniMax-H3 1024 px 6 steps 2.9 s a step. | Video is minutes per second of footage at low resolution. |

Outside this repository (local to the owner, read for this plan):

- **ABP's Video Studio plan** (`PLAN-video-studio.md`, phases V0 to V5, the Higgsfield feature list, the V3 likeness policy). V0
  found the only working route on the current driver: stable-diffusion.cpp on Vulkan with cooperative matrices disabled
  (they crash it). Wan 2.2 TI2V-5B Q8: 320x192x9 frames 4 steps 66 s; **1280x704x33 frames 4 steps 1101 s (244 s a step)**. Native
  720p / 121 frames / 30 steps would be 8 to 12 hours. ABP's V1 engine (Python, committed on ABP branches) already has a durable
  job queue, versioned workflow templates with typed parameters, a SHA-256 model registry, a media layer, a library with
  lineage and provenance. **Those designs are the spec to port; the Python is not.**
- **AI Transcriber & Caption Maker** (PyQt6 + faster-whisper, 3.2 kLOC): exporters (SRT, VTT, ASS, JSON), caption burn-in through
  ffmpeg, an optional pyannote diarizer, an editing timeline. Its transcription core is what MM's `speech.*` already does; its
  **ASS export, burn-in, word grouping, subtitle import and editor UI** are what MM lacks.

## 3. What the research changes

1. **"Watching a video" is mostly wiring, not new engines [repo].** llama.cpp already decodes video (through ffmpeg) and takes
   audio. The first win is an ffmpeg component, a capability flag and a pass-through test. Phase 1 is small.
2. **OpenCV 5 is for classic vision and I/O, not for the deep models [web].** OpenCV 5.0 (June 2026) has a rewritten DNN engine
   (over 80% of ONNX operators, tokenizers and KV-cache for small LLMs/VLMs) that is **CPU-only at release**. MM already has ONNX
   Runtime and llama.cpp for deep models, so OpenCV earns its place with what they lack: video I/O, scene and motion analysis,
   optical flow, tracking, stabilisation, contours and geometry, feature matching, calibration, and the new 3D stack (point
   clouds, meshes, RGB-D reconstruction). The Rust binding `opencv` (0.102.0) lists 5.x as supported [web]; building it needs the
   OpenCV SDK and libclang. **[spike]** It builds here against the OpenCV 5.1.0 SDK for the modules the worker needs (core,
   imgproc, videoio) with one compiler flag, but not for `video` and `dnn` (section 6, S1).
3. **Do not feed every frame to a VLM.** A tiered cascade (cheap gating, OCR, small VLM on keyframes, big model only for
   answers) is the only way live video fits one 24 GB card shared with chat models. See 4.3.
4. **The generation bottleneck is the compute path, not the model [call].** Vulkan with cooperative matrices off is slow
   (244 s a step at 720p). The Forge has PyTorch ROCm working on the same card, with a measured 91 TFLOPS bf16 matrix product
   [repo]. ComfyUI Desktop supports ROCm on Windows officially [web]. If diffusion through ROCm is several times faster than
   sd.cpp on Vulkan, the backend strategy flips. **Spike S4 measured it: 43x faster per step on the V0 shape (5.6 s against
   244 s) [spike]; the VAE decode is the unsolved part (section 6). The plan keeps both backends behind one interface.**
5. **3D generators are CUDA-first [web].** Hunyuan3D 2.1: about 10 GB for shape, 21 GB for texture, 29 GB for both (README).
   TRELLIS.2 (4B, MIT): verified on A100 and H100, 24 GB NVIDIA, Linux. ROCm forks of Hunyuan3D exist. So 3D on this card is a
   risk to be measured (spike S5), and the plan needs a second route that does not depend on it: **programmatic 3D** (a model
   writes Blender, OpenSCAD or CadQuery code; deterministic, precise, runs anywhere). A router chooses by intent. **[spike]
   TripoSR and Hunyuan3D 2.1 shape generation both run on this card (a mesh in seconds and in 45 s); texture was not tried.**
6. **ASR has three viable engines already within reach [web, repo].** whisper.cpp (99 languages, in MM), Parakeet TDT 0.6B v3
   through ONNX (25 European languages, far faster on CPU; third-party figures of about 18x real time on an old CPU), and the
   llama.cpp audio models (Qwen3-ASR, Voxtral, Gemma 4). Which wins is a benchmark on the owner's own audio (spike S3), not a
   decision for this document. Diarization: Sortformer (streaming, at most 4 speakers) or pyannote segmentation plus a speaker
   embedding with clustering, all ONNX. **[spike] On a synthetic meeting: whisper.cpp on Vulkan 60x real time, Parakeet on CPU
   17.7x, whisper on CPU 0.9x; pyannote + WeSpeaker diarization right on every segment; Gemma 4 hears but cannot tell speakers
   apart (section 6, S3). Real audio is still to be run.**
7. **Capture is solved by crates [web].** `windows-capture` (Windows.Graphics.Capture and DXGI), `scap`, WASAPI loopback for
   system audio. A meeting assistant needs no bot that joins a call: it records the local screen and the local audio.

## 4. Design

### 4.1 Principles

- MM stays a hub. Heavy things are **separate processes** (llama-server, sd-cli, the Forge worker, and one new media worker),
  started and accounted for by the hub, like the engines today. The main binary does not link OpenCV or ffmpeg.
- **Everything optional installs as a setup component** (the mechanism `prism-gpu` uses in `src/misty/setup.rs`), with pinned
  versions and SHA-256, and the feature degrades with a clear message when it is absent (the `501` pattern of `whisper` and
  `vision`).
- **Every capability is an operation first** (so it is also an MCP tool and a Misty tool), then an HTTP route. OpenAI-shaped
  routes where an OpenAI shape exists (`/v1/audio/transcriptions`, chat with `video_url`), MM routes otherwise.
- **Measure before choosing a model.** A small eval harness (a video-question set, a meeting recording with a known
  transcript, a screen-recording set) lives in the repo; model choices in the registry cite its numbers, as `benchmark.rs`
  already does for chat models.
- **One GPU, shared** (the owner's rule: 2 GB left for the system, models up to 22 GB). Every media job goes through the
  hardware-aware loader and the existing approval queue; nothing unloads a chat model without the policy in decision D3.

### 4.2 Processes

```
                 hub (model-mistress)  -- ops / MCP / HTTP / jobs / asset store / timeline index (SQLite)
                   |        |         |            |
       llama-server   sd-cli   mm-media-worker   forge worker (PyTorch ROCm, optional)
       (VLM, audio    (image,   (OpenCV, capture,  (ComfyUI or diffusers: models sd.cpp
        LLMs, chat)    video)    VAD/ASR/diarize     lacks; 3D; Wan Animate, camera)
                                 via ort, keyframes)
                          ffmpeg + ffprobe (pinned LGPL build, subprocess)
```

- **`mm-media-worker`** is a new Rust workspace member (not in the main binary). It links OpenCV, reads frames through ffmpeg
  or OpenCV's own VideoCapture, captures the screen and audio on Windows, runs the ONNX models (VAD, ASR, diarization, OCR,
  depth, matting) through `ort`, and talks to the hub over a framed stdio or local-socket protocol. A crash or a leak costs
  one process, not the hub. Frames never cross HTTP as base64.
- **ffmpeg** is a pinned **LGPL** build (no x264/x265, so redistribution stays simple; AMD's AMF encoders are in LGPL builds
  [call, verify]). Needed for llama.cpp's video input, any container, muxing, burn-in, HLS.
- **The Forge worker** is reused for what only PyTorch can do (see 4.6 and 4.7).

### 4.3 Perception pipeline ("see and hear" a file, a stream or a capture)

```
source -> demux -> audio:  VAD -> ASR (words) -> diarization -> speech events
                -> video:  Tier 0 gate -> Tier 1 OCR/UI -> Tier 2 caption (small VLM) -> frame events
          all events -> timeline (SQLite + FTS5 + embeddings) -> hierarchical summaries -> questions, notes, search
```

- **Tier 0, OpenCV, CPU, every frame sampled at 2 to 5 fps:** scene-change (histogram and structural difference), motion,
  slide or window change, near-duplicate removal (perceptual hash). Keeps roughly 0.05 to 1 frame per second of interest.
- **Tier 1, text and structure:** OCR on frames that contain text (screen shares, slides), UI element detection for screens.
- **Tier 2, a small VLM** (candidates: Qwen3-VL 2B/4B/8B, Gemma 4 E4B, SmolVLM2; chosen by eval) captions the kept keyframes
  with the transcript around them as context. Qwen-style video input (several frames as one clip) is compared with
  keyframes-as-images in spike S2.
- **Tier 3, the chat model the user picked** answers questions over retrieved evidence (transcript spans, captions, OCR,
  the relevant keyframes) and cites timestamps. It never sees the whole video.
- **Timeline events** (one table, one JSON shape): `{t0, t1, kind: speech|frame|ocr|object|scene|summary, speaker?, text,
  confidence, asset?}`. Keyframes are stored as files in the asset store. Summaries are hierarchical (10 s, 1 min, 10 min,
  session), so a three-hour meeting is answerable without rereading it. Embeddings go through `/v1/embeddings` and **CacheIt**
  (a file already analysed costs nothing the second time: results are keyed by content hash and pipeline version).
- **Output:** answers with timestamps, chapters, notes (decisions, action items, open questions, who said what), SRT, VTT and
  ASS captions, a JSON timeline for other tools.

### 4.4 Live: streams, screen and meetings

- **Sources:** a file, a URL (yt-dlp or streamlink as optional components, ffmpeg underneath), a window or monitor
  (Windows.Graphics.Capture), the microphone, **system audio (WASAPI loopback)**, a camera. DRM-protected streams are not
  supported and not worked around.
- **Meetings, no bot:** the microphone channel is "me", the loopback channel is "everyone else", which gives a first split of
  speakers for free; diarization separates the remote voices; an optional voice enrolment names the owner. Screen frames are
  gated by slide-change detection and read by OCR. After the meeting: notes, decisions, action items, a searchable record.
- **Live loop:** a ring buffer feeds the same pipeline in windows. ASR is whisper on the GPU (Vulkan: 60x real time and under
  1 GB, so it shares the card with a chat model; S3), with Parakeet on the CPU (17.7x) as the fallback when the card is full;
  diarization runs on the CPU (10x). The small VLM runs only when Tier 0 says something changed. Rolling summaries answer
  "what did I miss".
- **Assistant behaviour:** triggers ("tell me when the build finishes", "when someone mentions the budget"), questions at any
  time (`watch.ask`), a notes document that grows during the session.
- **Consent and privacy:** local only, nothing leaves the machine; a visible recording indicator and a notification are on by
  default; the setting to turn them off is the owner's, and the law on recording others varies by place, so it is stated in the
  docs and not decided by code.

### 4.5 Operations and routes (names are proposals)

| Operation | Does |
|---|---|
| `media.components`, `media.probe` | what is installed (ffmpeg, worker, models); container, streams and duration of a file |
| `speech.transcribe` (extended) | any container; `words`, `diarize`, formats incl. **ass**; engine `whisper`, `parakeet` or `llm` |
| `video.analyze` (job) | file or URL to timeline; options: fps, tiers, models; resumable |
| `video.ask`, `video.notes`, `video.search` | questions with timestamps and frames; notes and chapters; search inside or across the library |
| `watch.start`, `.stop`, `.status`, `.ask`, `.notes` | live sessions on a screen, window, stream or meeting |
| `cv.scenes`, `cv.keyframes`, `cv.ocr`, `cv.track`, `cv.diff`, `cv.stabilize`, `cv.matte` | the toolbox for Misty and MCP clients |
| `studio.generate` (job), `studio.plan`, `studio.post`, `studio.models`, `studio.assets.*` | video generation, shot lists, upscale and friends, the library |
| `asset3d.generate` (job), `asset3d.refine` | image or text to 3D; mesh clean-up, UVs, export |

Chat with `video_url` and `input_audio` parts goes straight to llama-server for models that take them (Phase 1). Routes live
under `/v1/video/...`, `/v1/watch/...`, `/v1/studio/...`; peer keys get the read-only ones.

### 4.6 Studio: video generation (the Higgsfield replacement)

Port the **designs** of ABP's V1 and V3 into Rust, in MM, so every client (ABP, the desktop app, MCP) uses one engine and one
GPU queue. ABP's `bot/video` becomes a thin client afterwards (decision D4).

- **Jobs:** durable, resumable, with progress, cancel and a timeout; step-level checkpoints for multi-shot work (a shot that
  finished is never rendered again). Builds on MM's job system (`image.generate` is already a job).
- **Backends behind one trait:** `sdcpp` (exists; the complete fallback), `diffusers` in the Forge worker (**the fast path for
  Wan: 43x faster per step in S4**, subject to the VAE issue there; `comfy` is the alternative if a workflow needs it), and
  opt-in **provider adapters** for frontier models (Veo, Kling and the like) with cost shown first, **never a default**
  (the owner's standing rule in the ABP plan).
- **Workflow templates:** JSON with typed, validated parameters (ABP's format), versioned, one per mode: text to video, image
  to video, first and last frame, video to video, extend, motion reference. A **model registry** with SHA-256, licence and
  measured speed per model (ABP's `models.json` format).
- **Planner:** a local LLM turns an idea or script into a shot list (camera, lens, duration, continuity notes) the user edits;
  the runner renders shot by shot and joins them with transitions and audio.
- **Camera and look:** three layers, cheapest first. (1) Prompt-level camera and lens presets, stackable. (2) **Deterministic
  effects with no diffusion:** depth (a depth model through ONNX) plus matting plus OpenCV warps give parallax, dolly zoom,
  orbit-lite, bullet-time-lite, 3D rotation, rack focus, film grain, anamorphic bloom. They are seconds, not hours, and they
  are the answer to most of Higgsfield's effects library [call]. (3) Camera-conditioned models (Wan 2.2 Fun Camera Control,
  Apache-2.0 per ComfyUI's docs [web]) through the Forge worker.
- **Consistency:** reference-image conditioning first (Wan VACE, which sd.cpp supports [repo], and image edit models), then
  character LoRA training (reuses the Forge). Wan Animate (driving-video character animation [web]) through the Forge worker.
- **Post:** upscale with SeedVR2 or RelayVSR (already in sd.cpp [repo]) and Real-ESRGAN or RIFE through ncnn on Vulkan (MIT
  wrappers [web]); frame interpolation; matting; stabilisation (OpenCV); colour and grain (ffmpeg filters).
- **Audio:** LTX-2.x makes sound with the picture [repo]; speech from MM's TTS; lip-sync through a speech-to-video model
  (Wan S2V, in sd.cpp per ABP's notes); music and sound effects through open models, chosen by spike.
- **Draft then refine** is the speed answer on this hardware: render short, low-resolution drafts with few-step models, let the
  user pick, then upscale or re-render only the keeper. The UI shows measured time per model before a render starts.
- **Likeness and provenance:** the ABP V3 policy carries over unchanged: consent record per real person, face match against it,
  refusals (sexual content, minors), a visible label and embedded provenance on every output, an append-only audit log, off by
  default. InsightFace models are non-commercial research, so the swap stays marked as such.

### 4.7 3D assets

Two routes, chosen by a router on the request:

- **Generative (organic shapes, props, characters):** concept image (sd.cpp: FLUX, Qwen-Image, MiniMax-H3) -> matting ->
  image-to-3D in the Forge worker (**TripoSR** for a draft in seconds and **Hunyuan3D 2.1** shape for a better mesh in under a
  minute both ran in S5; TRELLIS.2 is untested) -> mesh clean-up, decimation, UV unwrap -> texture (open, see S5) -> **GLB**,
  shown in the existing chat canvas. Multi-view and video-to-3D come later (OpenCV 5's 3D
  stack for reconstruction from a walk-around video).
- **Programmatic (precise, parametric, game-ready primitives):** the model writes Blender (headless, optional component),
  OpenSCAD or CadQuery code in the sandbox; the hub runs it, renders previews, and loops on its own errors. Exact dimensions,
  holes, tolerances: this route beats any diffusion model at them.
- Everything lands in the asset library with its lineage (prompt, image, seed, model, code), licence and provenance.

### 4.8 Resource budget

| Resident or on demand | Where | Rough size [call] |
|---|---|---|
| Chat model(s) | GPU, as today | up to 22 GB (the owner's cap) |
| "Eyes" VLM, small | GPU or CPU | 2 to 6 GB; loaded on demand, unloaded when idle |
| VAD, ASR, diarization, OCR, depth, matting | CPU (ONNX), 12 cores | under 3 GB RAM together |
| Video generation, 3D | GPU, **exclusive**, as a job | 13 to 24 GB; queue waits for the card |
| Models on disk | NVMe preferred (load time dominates: a 12 GB set took about 3 min from an HDD and 25 s from NVMe in ABP's V0) | tens of GB each; see D6 |

### 4.9 Relation to the other projects

- **ABP Video Studio:** its plan stays the product spec (features, phases V0 to V5 of generation, the likeness policy). Its V0
  measurements are inputs here. Decision D4 says who owns the engine.
- **AI Transcriber & Caption Maker:** keep it as an application, but make it a **client of MM** (one model stack, one GPU
  scheduler). Port its pure logic (ASS styling, word grouping, subtitle import, burn-in arguments) to a Rust `captions` module in
  MM so every client gets them.
- **CacheIt:** cache embeddings, OCR and caption results by content hash; video frames are the biggest win it will see.
- **Misty:** every `cv.*`, `video.*` and `watch.*` operation is a Misty tool; the live assistant is a Misty agent with a
  `watch` session as its senses.

## 5. Decisions for the owner

| # | Decision | Recommendation |
|---|---|---|
| D1 | OpenCV packaging: **(a)** worker process, **(b)** linked into the hub behind a feature, **(c)** Python (opencv-python) in the Forge | **(a)**: keeps the hub small and isolated; S1 shows the binding builds and matches Python's speed for the modules the worker needs. (c) stays right for flow-based effects until the binding links `video` |
| D2 | Run the spikes S1 to S6 before building | **answered: spikes first, run 2026-11-27**; results in section 6. Still open: real meeting audio for S3, the 30-minute soak for S6 |
| D3 | May a render or a 3D job unload a loaded chat model? (ABP needed the owner's direct OK each time) | the owner allowed it for the spike runs; for the product: ask once per session in the approval queue, remember "always" per model if the owner says so |
| D4 | Who owns the video engine: **MM** (ABP becomes a client) or ABP (MM stays a hub) | **MM**: one GPU queue, one asset library, every client benefits |
| D5 | The Transcriber becomes an MM client | yes |
| D6 | Disk tier for video, 3D and VLM models (tens of GB each; C: is excluded by the owner's rule) | decide before S4 and S5 download anything |
| D7 | Meetings: local-only, no joining bot, recording indicator on by default | yes |

## 6. Spike results (measured 2026-11-27)

Machine: Ryzen 9 5900X (12 cores), 64 GB RAM, RX 7900 XTX 24 GB, Windows 10, the engines bundled with MM (llama.cpp b12726).
Test material was synthetic on purpose (no private recordings): a 32 s "meeting" (two Windows text-to-speech voices, three
slides, a script with known facts) and a generated 1080x1920 60 fps clip. **Limits that apply to every number below:** one
machine, short clips, an easy question set. They rank options and expose failures; they are not a quality benchmark, and the
real-audio and long-recording runs the original S3 asked for are still open.

### S1, OpenCV 5 and the Rust binding

| Measured | Result |
|---|---|
| Python OpenCV 5.0.0 and Rust `opencv` 0.102.0 against the OpenCV **5.1.0** Windows SDK, same 33 s 1080x1920 60 fps clip | decode of every frame **4.2x (Python) and 5.7x (Rust) real time**; sampling at 2 fps with grab and retrieve **27.1x and 26.6x** real time. The binding costs nothing over Python, as expected (same C++). |
| A real phone recording (1080x1920, 60 fps) with the Python build | full decode 1.6x real time, 2 fps sampling 15x: **decode is the bottleneck, analysis is not** (Tier-0 gate 2.4 ms per sample, DIS optical flow at 480p 10.4 ms per frame pair). |
| Footprint of the SDK's prebuilt libraries | `opencv_world510.dll` 83 MB, `opencv_videoio_ffmpeg510_64.dll` 41 MB. |
| Building the binding | Works only with these settings: `OPENCV_INCLUDE_PATHS`, `OPENCV_LINK_PATHS`, `OPENCV_LINK_LIBS=opencv_world510`, `LIBCLANG_PATH`, and **`OPENCV_CLANG_ARGS=-D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH`** (libclang 18 against an MSVC STL that wants Clang 19). |
| What does not build | The features `video` (which pulls in `dnn`) and `dnn` fail at link time: two symbols the 0.102.0 bindings call (`Net::argType`, `GlobalAveragePoolLayer::create`) are not in the 5.1.0 library. `core`, `imgproc`, `videoio` link and run. |

Verdict: **go on D1(a)**. The worker needs decode, resize, colour conversion, difference, histograms: all in the working
modules. Optical flow and the DNN module are not needed by Tier 0; flow-based effects run in the Forge's Python OpenCV (5.x
wheels exist) until the binding catches up. Pin the SDK and the flag in the setup component, and re-test on each OpenCV bump.

### S2, video understanding through the bundled llama-server

Native `input_video` (the server samples at `--video-fps 1` through ffmpeg) against **six keyframes sent as images**, five
questions whose answers are on the slides, ground truth known.

| Model (GGUF) | Takes | Load | VRAM (32k context) | Native video: tokens, first answer | Keyframes: tokens, first answer | Score video / keyframes |
|---|---|---|---|---|---|---|
| Qwen3-VL-4B Q4_K_M + mmproj Q8_0 | image, video | 10.6 s warm (38.7 s cold) | 9.5 GB (9.7 peak) | 9,738 tokens, 48 s | 3,100 tokens, 6.5 s | 5/5, 5/5 |
| Qwen3-VL-8B Q4_K_M + mmproj Q8_0 | image, video | 146 s cold from an HDD | 11.9 GB (12.1 peak) | 9,738 tokens, 41.5 s | 3,100 tokens, 8.6 s | 5/5, 5/5 |
| Gemma 4 E4B Q4_0 + mmproj Q8_0 | image, video, **audio** | 42.8 s | 6.0 GB (6.4 peak) | 13,123 tokens, 29.6 s; with its audio track 13,932 tokens, 26.2 s | 1,361 tokens, 2.3 s | 5/5, 5/5, with audio 5/5 |

Follow-up questions on the same video cost about 1 s (the prompt is cached); generation ran 60 to 110 tokens a second. The 4B
run overlapped two compiler jobs, so its first-answer times are pessimistic; the 8B and Gemma runs had a quiet CPU.

- **Native video works** once the server is given the file the right way (`--media-path` plus `file://name`) and the pinned
  ffmpeg. MM can pass `video_url` parts through.
- **The question set is too easy to rank the models on accuracy** (large slide text): all three score 5/5 in both modes. What
  separates them is cost. Phase 2's eval set must include small text, motion and scenes without text.
- **Cost decides the architecture.** A 33 s clip at 1 fps is 9.7 to 13 thousand tokens and 26 to 48 s of prefill: roughly **1
  s of compute per second of video** (0.8 to 1.5 s depending on the model), so feeding whole recordings to a VLM does not scale
  to an hour. Six keyframes carried the same answers in a fifth to a thirteenth of the time. This confirms the tiered cascade of
  4.3 (gate, keep keyframes, caption them).
- **Gemma 4 E4B is the best first "eyes and ears" model** on this card: the smallest (6 GB), the only one that also hears, and
  the fastest on frames. Qwen3-VL-8B is the quality option; 4B sits between them in cost.
- **Audio inside a chat model, audio only:** Gemma 4 E4B transcribed the 32 s meeting with **0 word errors in 1.6 s** (about
  20x real time), but it **cannot be trusted with speakers**: it counted four speakers where there are two and attributed
  the decision to a man when a woman made it. Keep a dedicated diarizer (S3).

### S3, speech to text and speakers

| Engine | Where | Speed on the 32 s meeting | Quality |
|---|---|---|---|
| whisper.cpp through `whisper-rs` 0.16, large-v3-turbo q5_0 | CPU, 6 threads | **0.9x real time** (cannot keep up) | transcript correct |
| the same, `vulkan` feature | GPU (Vulkan, cooperative matrices off as for every other engine on this card) | **59.8x real time** after warm-up (0.54 s; the first run, with shader compilation, 8.1x) | identical transcript; 0.6 GB of weights |
| Parakeet TDT 0.6B v3, int8 ONNX through ONNX Runtime | CPU, 6 threads | **17.7x real time**, 6.9 s load | 3.1% word error against the script, token timestamps included |
| Gemma 4 E4B through llama.cpp (S2) | GPU | about 20x real time | 0 word errors; no timestamps, unreliable speakers |
| sherpa-onnx diarization: pyannote segmentation 3.0 + WeSpeaker ResNet34 | CPU, 4 threads | **10.4x real time** | all nine segments given to the right one of two speakers; turn boundaries within 0.8 s of the script |

- **GPU whisper changes the live design.** At 60x real time it needs about 1.7% of the card's time and under 1 GB, so live
  captions can share the GPU with a chat model; the CPU route of 4.4 stays as the fallback (Parakeet at 17.7x, whisper small
  models). `whisper-rs` already has the `vulkan` feature, so this is a build flag and a runtime choice, not a new engine.
- **Build caveat:** the Vulkan build of `whisper-rs-sys` fails when the build directory path is long (the shader sub-build
  hits the 260-character limit of the MSVC debug database); MM's short shared target directory avoids it.
- **Not run:** Qwen3-ASR, Voxtral and Sortformer; any real meeting audio (overlapping speech, noise, accents); the 10-minute
  test. Parakeet covers 25 European languages only, whisper 99: the router must choose by language.

### S4, diffusion throughput on PyTorch ROCm against sd.cpp on Vulkan

Wan 2.2 TI2V-5B with the **real Q8_0 weights** through `diffusers` 0.35.2 and `gguf` on PyTorch 2.9.1 + ROCm 7.2.1 (the
Forge's stack). One denoising step with classifier-free guidance is two forward passes. The text-encoder output was a random
stand-in, so this measures speed and memory, **not prompt following or picture quality**.

| One step at | sd.cpp on Vulkan (ABP V0) | PyTorch ROCm, Q8_0 weights | PyTorch ROCm, bf16 compute core only |
|---|---|---|---|
| 1280x704, 33 frames | **244 s** | **5.64 s** (43x faster) | 3.88 s |
| 832x480, 49 frames | not measured | 2.98 s | 2.10 s |
| 832x480, 81 frames | not measured | not measured | 4.05 s |
| 1280x704, 81 frames | not measured | not measured | 17.4 s |
| 1280x704, 121 frames (5 s at 24 fps) | est. 8 to 12 h for 30 steps | not measured | 48 s |

- The Q8 weights load in 14 s and take 5.2 GB (5.9 GB at the 720p step). Keeping them quantised and dequantising per layer
  costs about 45% against the bf16 core. A 4-step 1280x704x33 clip is therefore about 23 s of denoising where sd.cpp needs
  1,101 s. Extrapolating the core numbers, **30 steps of 720p at 121 frames is roughly 25 to 35 minutes, not hours** [call].
- A bf16 matrix product reaches 91 to 98 TFLOPS on this card, so the hardware is not the limit; the Vulkan path is.
- **The VAE decode is the weak point.** The Wan 3D convolutions go through MIOpen on this stack, and MIOpen fails or crawls:
  with default settings the first decode of 832x480 at 33 frames spent about ten minutes of single-core CPU compiling kernels
  and then failed; with MIOpen's fast find mode a 17 frame 832x480 decode took 32 s (3.5 s at 576x320), and every larger
  size failed with "HIP out of memory" on a 3 to 5 GiB request while 17 GiB was free, in every MIOpen setting tried
  (default, fast, GEMM solvers off). **Turning MIOpen off for convolutions
  (`torch.backends.cudnn.enabled = False`) fixed it: 832x480 at 17 frames 3.9 s, at 33 frames 6.5 s**, 15 GB peak. At
  1280x704 the untiled decode needs more than the card has, and the tiled decode did not finish in ten minutes (a
  single-core stall that was not diagnosed). **A 720p decode is therefore unsolved.** The Wan 2.2 VAE file also does not load
  through diffusers' single-file converter, so the decode runs used random weights (speed does not depend on them).
- Operational: a PyTorch ROCm process kept 5.7 GB of commit memory after printing its final result, so the hub must supervise
  and end worker processes itself.
- **Not measured:** the umt5 text encoder through PyTorch, LTX-2.x, ComfyUI, any output frame, a 30-step run.

Verdict: the 43x gain on the denoising loop is large enough to **flip the backend order for Wan**: diffusers on ROCm in the
Forge becomes the fast path and sd.cpp stays the complete, slower fallback (it also covers every model the Forge lacks). The
first task of Phase 4 is an end-to-end Wan 2.2 5B run at 832x480 (denoise 3 s a step, decode 6.5 s, within what was measured),
compared frame by frame with sd.cpp; 720p waits for the VAE fix (tiling that works, a CPU decode, or a newer ROCm wheel).

### S5, 3D feasibility

A concept image from **MM's own sd-cli** (SDXL-Turbo, 4 steps, 12 s) of a wooden toy sailboat, cut out with GrabCut, then
image to 3D on PyTorch ROCm. Meshes were checked by rendering point views from four angles.

| Model | Load | VRAM | Time | Result |
|---|---|---|---|---|
| TripoSR (MIT) | 10 to 23 s | 1.6 GB weights, 2.4 GB peak | **0.7 s** image to triplane (2 to 5 s first run) plus **5 s** mesh extraction (CPU marching cubes at 256^3, with colours) | 28,750 vertices, 57,496 faces, watertight, vertex colours; recognisable boat with hull, mast and a thin flat sail |
| Hunyuan3D 2.1, shape only (Tencent community licence) | 6.6 s | 6.9 GB weights, 7.7 GB peak | **45 s** warm (48 s first) for 30 steps at octree 256 | 145,086 vertices, 290,176 faces, watertight, clean silhouette from every angle, no texture |

- **Image to 3D shape generation works on this card today:** seconds for a draft (TripoSR), under a minute for a better
  shape (Hunyuan3D). The chain concept image, cut-out, mesh uses MM's existing image engine for step one.
- **Texture is the open problem and was not run.** Hunyuan3D's paint stage needs a CUDA-only rasteriser extension
  (about 700 lines of CUDA and C++) and a large multi-view paint model (its README quotes about 21 GB for texture). The ROCm
  SDK wheels include `hipcc` and `hipify-clang`, so a port looks possible, and 21 GB is tight on 24 GB. It needs its own spike;
  until then the generative route delivers untextured or vertex-coloured meshes (TripoSR's colours are usable for drafts).
- **Integration gotchas found:** the upstream Hunyuan3D loader builds its 3.3-billion-parameter model in fp32 on the CPU first
  (about 13 GB of commit) and failed with "not enough memory" at 18 GB of free commit; building it on the card in fp16 fixed it.
  TripoSR's marching-cubes dependency needs a C++ build; scikit-image on the CPU replaced it. `torchvision` must be AMD's ROCm
  wheel (the PyPI CPU wheel does not load against ROCm PyTorch). Both models run from one Python environment next to the Forge's.
- Verdict: **go for the generative route as draft and refine** (TripoSR first, Hunyuan3D shape on request), with the
  programmatic route unchanged; add a texture spike before promising textured assets.

### S6, capture

| Measured | Result |
|---|---|
| Screen capture, Windows.Graphics.Capture through `windows-capture` 2.0.1, 1920x1080 | **58 fps** (the display's refresh rate); mapping each frame and scanning it sparsely 2.2 ms; about 3% of one core. |
| System audio, WASAPI loopback through `cpal` 0.15 (an input stream opened on the output device) | works at the device's 48 kHz stereo float; a faint test tone (about -48 dBFS) was read back at its exact level, so the signal path is proven. |
| Gotcha | `cpal` and `windows-capture` both initialise COM; on one thread the capture fails with `FailedToInitWinRT`. **The audio loopback runs on its own thread.** |
| Not measured | the 30-minute soak and audio and video sync (original S6 exit criteria). |

Verdict: capture is cheap and works; build the worker's capture module on these two crates with the two-thread layout.

## 7. Phases

**Phase 0, spikes (decide, do not build).** Each ends with a written result in `docs/` and numbers. Results are in section 6;
the descriptions below are what each spike set out to do and what is still open.

- **S1, OpenCV 5 + opencv-rust builds** with MSVC and libclang on this machine; reads a video, finds scene cuts, runs a DNN
  model; DLL footprint. *Exit:* a 60-line program does it; go/no-go on D1(a).
- **S2, video into llama-server:** ffmpeg on PATH, Qwen3-VL (4B and 8B) and Gemma 4 E4B, native video input against N
  keyframes as images; tokens per frame, time, VRAM, answer quality on a fixed question set.
- **S3, ASR and diarization bake-off** on 10 minutes of the owner's real meeting audio: whisper.cpp (CPU, and Vulkan if the
  crate exposes it), Parakeet v3 ONNX, Qwen3-ASR and Gemma 4 through llama.cpp; Sortformer against pyannote + embeddings.
  Real-time factor, WER read by eye, speaker errors.
- **S4, diffusion throughput:** Wan 2.2 5B and LTX-2.3 through ComfyUI/diffusers on PyTorch ROCm in the Forge, against
  sd.cpp Vulkan, same prompt and size (the V0 baseline: 244 s a step at 1280x704x33).
- **S5, 3D feasibility:** TripoSR, then Hunyuan3D 2.1 shape-only, then texture, on ROCm; VRAM, time, mesh quality.
- **S6, capture:** Windows.Graphics.Capture plus WASAPI loopback in Rust: frame rate, CPU cost, audio sync, a 30-minute soak.

**Phase 1, native video and audio in chat (days).** ffmpeg component with pinned LGPL build (passed to llama-server with
`--video-ffmpeg-dir`; the server is started with `--media-path` so local files go in as `file://name`, S2); `media.probe`;
`speech.transcribe` reads any container through ffmpeg, gains `words` and `ass`, and offers the Vulkan whisper build (S3);
chat `video_url` and `input_audio` parts reach llama-server and the models list advertises the capability. *Exit:* a Qwen3-VL or Gemma 4 chat answers a question about a short MP4; tests with a
fixture clip; docs and CHANGELOG.

**Phase 2, perceive a file (weeks).** `mm-media-worker`, Tier 0 to 2, timeline store, `video.analyze`, `video.ask`,
`video.notes`, `video.search`, CacheIt caching, the eval harness. *Exit:* a one-hour recording becomes a searchable, cited
timeline in a measured time; the eval set has recorded scores.

**Phase 3, live and meetings (weeks).** Capture sources, VAD and streaming ASR, rolling summaries, `watch.*`, the meeting
notes flow, a panel in the desktop app and dashboard, Misty tools. *Exit:* a 30-minute call is transcribed live with speakers,
and the notes appear within a minute of the end; the card stays usable for chat meanwhile.

**Phase 4, Studio (weeks, parallel to 3).** Asset library and lineage, templates and registry, shot planner, `studio.generate`
through sd.cpp first and, in parallel, the end-to-end Wan run in the Forge (S4: fast path at 832x480, 720p after the VAE fix),
post tools, the effects library, audio, likeness policy.
*Exit:* a 3-shot scene with sound from one brief, rendered locally, with provenance, resumable after a restart.

**Phase 5, 3D.** Programmatic route first (it works everywhere), then the generative route (S5 passed for shapes; add a texture
spike first). *Exit:* a GLB from a text request in the canvas, both routes.

**Phase 6, production grade.** Scheduled quality evals (prompt match by a VLM, temporal consistency, artefact checks), signed
C2PA provenance, content filters, provider adapters (opt-in), cluster rendering, crash recovery, dashboards.

## 8. Risks and licences

- **ROCm on this card for diffusion and 3D** is proven for transformer denoising and 3D shapes (S4, S5) and **not** for the Wan
  VAE at 720p (MIOpen's 3D convolutions fail or crawl; native convolutions work to about 832x480x33). The plan survives because
  sd.cpp stays a backend and the programmatic 3D route needs no ROCm. Windows driver and ROCm wheel updates can change any of
  this, so the numbers in section 6 are re-run on each Forge upgrade.
- **Video generation will stay slow** (minutes per second of footage at low resolution). The design answers with drafts,
  few-step models, upscaling and deterministic effects, not with promises.
- **OpenCV 5 binding maturity** (the crate marked 5.x preliminary in earlier releases [web]); S1 is the gate.
- **Disk and commit memory:** rustc already failed once with a 27B model resident (HANDOVER); media jobs must respect the
  commit-aware loader.
- **Licences are per model and recorded in the registry, shown before download:** OpenCV Apache-2.0; ffmpeg LGPL build only;
  Wan Apache-2.0 [repo, ABP registry]; LTX-2 community licence; HunyuanVideo and Hunyuan3D carry Tencent community licences
  with territory and scale limits (read before use) [call]; TRELLIS.2 MIT [web]; InsightFace non-commercial; some GUI-grounding
  models are non-commercial (KV-Ground-4B is CC-BY-NC-SA [web]). Nothing is bundled without its licence being checked.
- **Safety:** likeness policy, content filters on prompts and outputs, provenance on everything generated, recording
  indicators, no DRM circumvention, no meeting bots.

## 9. Not decided here on purpose

Which depth and matting models win (an effects spike); the VLM, ASR and diarizer are narrowed by S2 and S3 (Gemma 4 E4B first
with Qwen3-VL as the quality option; whisper on Vulkan plus Parakeet; pyannote and WeSpeaker) but their final ranking waits
for the Phase 2 eval set and real audio; whether Studio's Forge backend is ComfyUI or diffusers (S4 ran diffusers; ComfyUI was
not tried); how the Wan VAE is decoded at 720p; how 3D is textured; the UI layout of the Studio and the watch panel (a design
pass after Phase 2).

## 10. Sources

- OpenCV 5.0: [Phoronix](https://phoronix.com/news/opencv-5.0-released), [heise](https://heise.de/-11326252), [OpenCV](https://opencv.org/opencv-5/)
- Rust binding: [opencv-rust](https://github.com/twistedfall/opencv-rust), [docs.rs 0.101.1](https://docs.rs/crate/opencv/0.101.1)
- llama.cpp multimodal (image, audio, video): the pinned tree's `docs/multimodal.md` and `tools/mtmd/` [repo]; [upstream guide](https://github.com/ggml-org/llama.cpp/blob/master/docs/multimodal.md)
- ASR: [Parakeet vs Whisper vs Nemotron](https://openwhispr.com/blog/parakeet-vs-whisper-vs-nemotron), [parakeet-rs](https://docs.rs/crate/parakeet-rs/latest/source/examples/diarization.rs), [parakeet-tdt-0.6b-v3 ONNX](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx)
- Capture: [windows-capture](https://docs.rs/crate/windows-capture/2.0.0), [scap](https://docs.rs/crate/scap/0.0.8/source/README.md), [win-native-media](https://docs.rs/win-native-media)
- ComfyUI on AMD Windows: [ComfyUI blog](https://blog.comfy.org/p/official-amd-rocm-support-arrives), [AMD ROCm blog](https://rocm.blogs.amd.com/artificial-intelligence/comfyui-windows/README.html), [community 7900 XTX write-up](https://joshwaamein.github.io/posts/comfyui-amd-rx7900xtx-rocm-windows/)
- Wan camera control and Animate: [ComfyUI docs](https://docs.comfy.org/tutorials/video/wan/wan2-2-fun-camera), [Animate](https://docs.comfy.org/tutorials/video/wan/wan2-2-animate)
- 3D: [Hunyuan3D 2.1](https://arxiv.org/html/2506.15442v1), [TRELLIS.2 model card](https://huggingface.co/microsoft/trellis.2-4b)
- Post: [rife-ncnn-vulkan](https://github.com/nihui/rife-ncnn-vulkan), [Video2X](https://github.com/k4yt3x/video2x)
- GUI grounding: [POINTS-GUI-G](https://huggingface.co/tencent/POINTS-GUI-G/blob/main/README.md), [MolmoPoint-GUI](https://allenai.org/blog/molmopoint)
