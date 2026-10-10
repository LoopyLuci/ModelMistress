# Hosting models for other machines

ModelMistress can serve its models to other machines (peers) with a key for each peer, quotas, a list of models each
key may use, usage accounting, and a hot swap that drops no request.

## Safe by default

The hub listens on loopback (`127.0.0.1`) only. To listen on another address (`serve --host 0.0.0.0`) two things
must be true, or `serve` refuses to start and says which:

1. `allow_network = true` in `<home>/config.toml` (or `model-mistress call config.set '{"settings":{"allow_network":true}}'`).
2. At least one active peer key.

Peers use their keys. The hub's own token (in `control.json`) is the owner's and does everything, but it is
accepted from this machine only: from any other address it answers `403`. Put a TLS-terminating reverse proxy in front of the hub if the network is not yours;
the hub speaks plain HTTP.

## Keys

```bash
model-mistress keys create alice --requests-per-min 60 --tokens-per-day 200000 --concurrent 2 \
    --model llama3.2:1b --model "qwen3*"
model-mistress keys list
model-mistress keys revoke alice          # a key id, or a peer name (all of that peer's keys)
model-mistress usage                      # or: usage alice
```

`keys create` prints the key (`mmk_...`) once. `<home>/keys.json` keeps only its SHA-256 hash, the peer's name,
the quotas and the allow-list; `<home>/usage.json` keeps the usage. With no hub running the commands work on those
files directly (the next `serve` reads them), so a first key can exist before the hub listens beyond loopback.

The same through the hub's operations: `key.create`, `key.list`, `key.revoke`, `usage.get`.

A peer sends its key like any OpenAI key: `Authorization: Bearer mmk_...` (or `x-api-key`). It may use
`/v1/models`, `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/rerank` and `/v1/audio/*`, and
nothing else: the control API answers `403`.

## Quotas

| Quota | Meaning |
|---|---|
| `requests_per_min` | requests in any sliding minute |
| `tokens_per_day` | prompt plus completion tokens per UTC day. Tokens are known only after the answer, so the request that crosses the line is served and the next ones are refused until midnight UTC |
| `concurrent` | requests at a time; a streamed answer counts until its last byte |

A request over a quota gets `429` with a `Retry-After` header (seconds) and
`{"error": {"code": "rate_limit_exceeded" | "token_quota_exceeded" | "too_many_concurrent_requests", ...}}`. A refused
request is counted in `usage` (`rejected`) and never reaches the model. Leave a quota out for no limit.

## Allow-lists

`models` lists the model ids a key may use: exact names (any case) or a prefix ending in `*`. Other models answer
`403 model_not_allowed`, and `/v1/models` shows the key only its own. No list means every model. Audio requests are
checked against the model name in the request, or `whisper-1` / `tts-1` when it names none.

A peer's request can load a model (autoload) and, with `max_loaded` reached, unload another: allow only what the
machine can hold, or turn `autoload` off and load models yourself.

## Revoking

`keys revoke` takes effect with the next request, on every route; requests already running finish. Revoked keys stay
in `keys list` and their usage stays readable.

## Usage

`usage` (`usage.get`) shows per key: `requests`, `errors`, `rejected`, `prompt_tokens`, `completion_tokens`,
`tokens_today`, `requests_last_minute`, `in_flight`, the quotas, `last_used` and `by_model`. Tokens come from the
answer's `usage`; for a stream without one, the number of events is counted instead.

## Hot swap

Replacing a model while peers are connected drops nothing. `model.load` with `"reload": true` (and a change of LoRA
adapters or draft model, which reload the model) starts the new copy beside the running one; when it answers, new
requests go to it and the old copy finishes what it is serving (streams included), then stops. If the new copy
does not load, the old one stays. Unloading and eviction (`max_loaded`) work the same way: a model is stopped when its
last request is done (at most 15 minutes after being replaced; shutdown does not wait). During the swap both copies
are in memory.

```bash
model-mistress call model.load '{"model": "llama3.2:1b", "ctx_size": 16384, "reload": true}'
```
