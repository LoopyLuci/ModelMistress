# llama.cpp's tools

Besides serving models, ModelMistress runs llama.cpp's tools. They are built from the vendored llama.cpp
(`engines/llama.cpp`) with the same build as `llama-server` (feature `bundled-engine`) and written next to it on
first start; nothing is downloaded. Without `bundled-engine` they are looked for beside the `llama-server` in use.

| Tool | Operation | CLI |
|---|---|---|
| `llama-quantize` | `quantize` | `quantize` |
| llama-server `--lora` and `/lora-adapters` | `lora` | `lora` |
| llama-server `--spec-draft-model` | `speculative` | `draft` |
| llama-server `--embeddings`, `/v1/embeddings` | `embeddings.create`, `POST /v1/embeddings` | `call` |
| llama-server `--reranking`, `/v1/rerank` | `rerank.run`, `POST /v1/rerank`, `POST /rerank` | `call` |
| `llama-tts` | `speech.synthesize`, `POST /v1/audio/speech` | `speak` ([text-to-speech.md](text-to-speech.md)) |
| `sd-cli` (stable-diffusion.cpp) | `image.generate`, `POST /v1/images/generations` | `image` ([images.md](images.md)) |
| ONNX Runtime (feature `vision`; not llama.cpp's) | `vision.analyze`, `POST /v1/vision/classify`, `/detect`, `/embed` | `vision` ([vision.md](vision.md)) |
| `convert_hf_to_gguf.py` | `model.convert` (answers 501) | |

## Quantize

```bash
model-mistress quantize llama3.2:1b Q4_K_M          # a model id from `models`
model-mistress quantize ./model-f16.gguf Q8_0 --out ./model-q8.gguf
```

The operation `quantize` takes `model` (an id) or `input` (a `.gguf` path), `type` and optionally `output`,
`overwrite`, `allow_requantize`, `leave_output_tensor`, `pure`, `imatrix`, `include_weights`, `exclude_weights`,
`output_tensor_type`, `token_embedding_type`, `threads` and `wait`.

- `type` is one of llama-quantize's own: `Q4_0 Q4_1 Q5_0 Q5_1 Q8_0 Q2_K Q3_K_S Q3_K_M Q3_K_L Q4_K_S Q4_K_M Q5_K_S
  Q5_K_M Q6_K IQ1_S IQ1_M IQ2_XXS IQ2_XS IQ2_S IQ2_M IQ3_XXS IQ3_XS IQ3_S IQ3_M IQ4_NL IQ4_XS TQ1_0 TQ2_0 MXFP4_MOE
  F16 BF16 F32 COPY` (and the aliases `Q3_K Q4_K Q5_K`), in any case. An unknown one is a `400` that lists them.
- `output` defaults to `<home>/models/<name>-<TYPE>.gguf`, a folder the catalog scans, so the result shows up in
  `model.list` when the job is done. A type already in the input's name is replaced (`model-F16.gguf` gives
  `model-Q4_K_M.gguf`). It must be a `.gguf` file, not the input, and not an existing file unless `overwrite`.
- The input must be 16-bit or 32-bit unless `allow_requantize` (quantizing a quantized model loses quality).
- It runs as a **job**, like a download: the answer is the job, `job.get` shows `percent` (`done_bytes` and
  `total_bytes` count tensors here, with the message "tensor 12 of 291"), `job.cancel` stops it and removes the
  partial file, and `wait: true` answers only at the end. The finished job's `result` has `output`, `input_bytes`,
  `output_bytes` and `seconds`. The CLI shows the progress.
- Errors: `404` no such model or file, `409` the output exists, `501` `llama-quantize` not found, `500` it failed
  (the job's message has the end of its output).

## LoRA adapters

A LoRA adapter is a small GGUF file made for one base model (`convert_lora_to_gguf.py` in llama.cpp makes them from
a Hugging Face adapter).

```bash
model-mistress lora qwen3:8b                              # what is attached
model-mistress lora qwen3:8b attach ./style.gguf --scale 0.8
model-mistress lora qwen3:8b scale style 0.3
model-mistress lora qwen3:8b detach style                 # or without a name: all of them
```

The operation `lora` takes `model`, `action` (`list`, `attach`, `detach`, `scale`), `adapter` and `scale`. An adapter
is a path or a file in `<home>/lora` (with or without `.gguf`).

llama-server loads its adapters when it starts (`--lora`, started switched off with `--lora-init-without-apply`, then
switched on at their scale through `POST /lora-adapters`). So `attach` and `detach` load a **loaded** model again with
the new set (`reloaded: true`); `scale` changes the scale in the running server without a reload. For a model that is
not loaded the change is kept and used the next time it loads. Adapters are kept per model in
`<home>/model_options.json`, so they survive an unload and a restart. If the new setup does not load, the old one is
restored and the error returned. `list` also shows the running server's own view (`live`). Adapters are not used on
a model loaded for embeddings or reranking.

## Speculative decoding (a draft model)

A small model drafts tokens that the big one checks, which makes generation faster when both share a vocabulary.

```bash
model-mistress draft qwen3:8b qwen3:0.6b --n-max 16
model-mistress draft qwen3:8b                  # shows the setting
model-mistress draft qwen3:8b --off
```

The operation `speculative` takes `model`, `draft_model` (a model id or a `.gguf` path) or `off: true`, and optionally
`n_max`, `n_min` (tokens drafted at a time) and `gpu_layers` (the draft model's layers on the GPU; -1 = auto, default
the same as the model's). Like LoRA, it is kept per model in `<home>/model_options.json` and a loaded model is loaded
again with `--spec-type draft-simple` (without it llama-server loads the draft model and never uses it), `--spec-draft-model`, `--spec-draft-n-max`, `--spec-draft-n-min` and `--spec-draft-ngl`. A draft model that
does not match fails the load; the old setup is restored and llama-server's message returned. Those option names are
the pinned llama.cpp's; another `llama_server` may call them `--model-draft` and `--draft-max`.

Speculative decoding is **off unless you set a draft model**, and `draft <model> --off` turns it off again. Measured on the
24 GB card it does not pay for a mixture-of-experts model such as Qwen3-Coder-30B-A3B (about 3 B active parameters, 150
tokens/s alone): the draft costs as much as the model, so the numbers in `docs/STATUS.md` show it slower. Try it on a dense
model that is slow on its own, and keep the card's memory free (the draft model and its cache must fit next to the model).

## Embeddings and reranking

Both are served by a model loaded for the purpose, next to the chat model: `max_loaded` (default 2) models stay
loaded, so a chat model and an embedding model fit together. A model is loaded for one use at a time (llama.cpp
serves embeddings or reranking only, then); asking a model that is loaded for another use gets a `409` that says
how to load it.

- `POST /v1/embeddings` (OpenAI) and the operation `embeddings.create`: `{model, input}`, `input` a string, a list of
  strings, or token ids; `encoding_format` `float` or `base64`. The model is loaded for embeddings on first use (or
  `model.load` with `embeddings: true`). It needs a model with a pooling type, such as nomic-embed-text or bge. Tested
  with nomic-embed-text-v1.5.
- `POST /v1/rerank` and `/rerank`, and the operation `rerank.run`: `{model, query, documents, top_n}`, `documents` a
  list of strings (or objects with a `text` field). The answer is llama-server's:
  `{model, usage, results: [{index, relevance_score}]}`, best first. The model must be a reranker (for example
  bge-reranker-v2-m3 in GGUF); it is loaded with `--reranking` on first use (or `model.load` with `reranking: true`).
  Not tested here: no reranker model was on the machine.

Bad bodies get a `400` before the model is touched; an unknown model a `404`.

## Converting Hugging Face weights

Converting Hugging Face weights to GGUF is a Python script (`convert_hf_to_gguf.py`) that needs PyTorch, so it is
**not built in**. ModelMistress has no Python environment manager, so `model.convert` answers `501` with what to
install:

```bash
python -m pip install torch transformers safetensors sentencepiece gguf numpy
python engines/llama.cpp/convert_hf_to_gguf.py ./my-model --outfile model-f16.gguf --outtype f16
model-mistress quantize ./model-f16.gguf Q4_K_M
```

Most models are already published in GGUF: `model-mistress search` and `pull` find them.

## Tests

Parsing, validation and argument building for every operation are unit tests and need no model. The tests that run a
real model are ignored unless you give them one, and need `--features bundled-engine` (a CPU build is enough:
`MM_GGML_BACKEND=cpu`):

```bash
MM_TEST_QUANTIZE_MODEL=/path/any.gguf cargo test -p model-mistress --features bundled-engine -- --ignored quantizes_a_real_model
MM_TEST_EMBED_MODEL=/path/nomic-embed-text.gguf cargo test -p model-mistress --features bundled-engine -- --ignored embeds_text
MM_TEST_LORA_MODEL=/path/model.gguf MM_TEST_LORA_ADAPTER=/path/adapter.gguf \
  cargo test -p model-mistress --features bundled-engine -- --ignored lora_on_a_real_model
MM_TEST_SPEC_MODEL=/path/model.gguf MM_TEST_SPEC_DRAFT=/path/draft.gguf \
  cargo test -p model-mistress --features bundled-engine -- --ignored speculative_decoding_on_a_real_model
```

Run the ignored tests one at a time or with `--test-threads=1` (each loads a model).
