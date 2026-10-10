# Image generation

ModelMistress draws images with stable-diffusion.cpp's `sd-cli`, built from the vendored tree
(`engines/stable-diffusion.cpp`, a pinned submodule with its own ggml, libwebp and libwebm) by the same build that makes
`llama-server` (feature `bundled-engine`). No Python, no separate server.

## Building

Nothing extra, except that the submodule is initialised with its own submodules:

```bash
git submodule update --init --recursive engines/stable-diffusion.cpp
cargo build --release -p model-mistress --features bundled-engine
```

`sd-cli` is a CMake project of its own (its ggml is a patched copy, so it can never share a library or symbol with
llama.cpp's). It follows the same switch as llama.cpp, `MM_GGML_BACKEND` = `vulkan` (default), `cuda`, `hip`, `metal`
or `cpu`, and `MM_CMAKE_GENERATOR` / `MM_CMAKE_ARGS`. WebP and WebM output are off: images are PNG. It is written to
`<home>/engines/bundled/<hash>/` on first start, next to `llama-server`. Without `bundled-engine`, ModelMistress looks
for `sd-cli` beside the `llama-server` it uses, or at the path in the environment variable `MM_SD_CLI`; if it is missing
the endpoint answers `501` and says so. On Windows a CMake build tree with long paths needs a short `CARGO_TARGET_DIR`.

## Why the command line program and not sd.cpp's server

`sd-cli` runs one request per process: the model (gigabytes) is loaded, used and freed, like `llama-tts`, so an idle hub
holds no image model in memory, cancelling is a plain kill, and progress is read from its output. `sd-server` would keep
a model loaded between requests, but then ModelMistress would have to manage another resident model and its VRAM next
to the chat models. Loading takes seconds against a generation of tens of seconds to minutes.

## Models

Put model files in `<home>/images/` (everything in it is an image model), or anywhere in the model folders if the file
name says what it is (`sdxl`, `sd3`, `sd_`, `flux`, `diffusion`, ...). The id is the file name without its extension; a
model id the catalog knows (a GGUF in a Hugging Face cache, say) and the path of a file work too. `model: "dall-e-3"`
(and the other OpenAI names) mean the default: the `image_model` setting, else the only image model found.

- **Whole checkpoints**: Stable Diffusion 1.x/2.x, SDXL, SD3 with its encoders inside (`.safetensors`, `.ckpt` or
  `.gguf`). Alone in a folder they are all that is needed; the tool takes them as `-m`.
- **FLUX** (and other models that come in parts) need extra files, which are paired **by name, from the model's own
  folder**, the way a vision model is paired with its `mmproj`:

  | File name contains | Option | Where to get it |
  |---|---|---|
  | `ae` (the whole name) or `vae` | `--vae` | FLUX.1's `ae.safetensors` |
  | `clip_l` | `--clip_l` | `clip_l.safetensors` |
  | `clip_g` | `--clip_g` | SDXL/SD3 text encoder |
  | `t5xxl` | `--t5xxl` | `t5xxl_fp16.safetensors` (or a quantized one) |
  | starts with `qwen` and contains `text_encoder`, `qwen_3` or `qwen2` | `--llm` | for models with an LLM text encoder |

  A model with a `t5xxl` or `qwen` encoder beside it, or `flux` in its name, is passed as `--diffusion-model`. Keep
  each such set in a folder of its own, for example `<home>/images/flux1-schnell/` with `flux1-schnell-Q4_K_S.gguf`,
  `ae.safetensors`, `clip_l.safetensors` and `t5xxl_fp16.safetensors`. FLUX wants `cfg_scale: 1` and (schnell) 4 steps.
  Preconverted FLUX GGUFs and the encoders are linked from `engines/stable-diffusion.cpp/docs/flux.md`.
- Models are never downloaded for you; `model.pull` only knows chat GGUFs.

## The API

`POST /v1/images/generations` (OpenAI): `prompt`, `model`, `n` (1 to 10), `size` ("512x512"; each side a multiple of 8
from 64 to 2048; "auto" is 512x512), `response_format` (`b64_json`, the default, or `url`), and the extensions `steps`
(1 to 150), `quality` (`low` 10 steps, `medium`/`standard` 20, `high`/`hd` 30, `auto`: the tool's own; `steps` wins),
`seed`, `negative_prompt`, `cfg_scale` and `sampler` (sd.cpp's sampling method, e.g. `euler`, `euler_a`, `dpm++2m`). The
answer is `{"created", "data": [{"b64_json"} | {"url"}], "model", "seed"}` with PNG images. `url` points at
`GET /v1/images/files/<name>` on the same hub (it needs the same bearer token); generated files are kept in
`<home>/generated/` and removed after a day.

```bash
curl -s http://127.0.0.1:PORT/v1/images/generations -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"prompt": "a red fox in snow", "size": "512x512", "steps": 20}'
```

`POST /v1/images/edits` answers `501`: sd.cpp can do image-to-image and inpainting, but OpenAI marks the part to change
by the mask's transparency, which would need a PNG decoder and the opposite mask polarity; it does not map cleanly.
A peer key may use the generation route (and the files route), like the other OpenAI routes. Requests run one at a time
(with the other tools); a client that disconnects stops its generation.

Errors: `400` a bad request, `404` no such model or no image model at all, `501` `sd-cli` not found, `500` it failed
(the message has the end of its output).

## Operation and CLI

The operation `image.generate` takes the same fields, plus `output` (the PNG path on the hub's machine; with `n` above 1,
`_0`, `_1`, ... go before `.png`; default: files in `<home>/generated/`) and `wait`. It runs as a **job**: `job.get` shows
the sampling step (`done_bytes` of `total_bytes` count steps, the message says "step 3 of 20"), `job.cancel` stops it,
and the finished job's `result` has `files`, `model`, `seed` and `seconds`.

```bash
model-mistress image "a red fox in snow" --out fox.png --model sdxl_base_1.0_Q4_K_S --size 512x512 --steps 20
model-mistress image "a lighthouse" --out l.png --model flux1-schnell-Q4_K_S --steps 4 --cfg-scale 1
```

Setting `gpu_layers` to 0 keeps image generation on the CPU too. The `image_model` setting names the default model.

## Tests

Request parsing, sizes, argument building, progress parsing and model pairing are unit tests. One test draws for real
(256x256, 4 steps), and is ignored unless a model is given:

```bash
MM_TEST_IMAGE_MODEL=<a model file> cargo test --features bundled-engine -- --ignored draws_with_a_real_model
```
