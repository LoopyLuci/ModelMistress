# ModelMistress Router API (`/mm/v1`, version `mm/1`)

The router is the headless ModelMistress build on an Android phone. Other apps on the phone, or on the same network when the
owner allows it, use it through two surfaces on one server:

- **OpenAI routes** at `/v1`: `health`, `models`, `chat/completions` (streamed or not), `images/generations`. A plain client
  that speaks the OpenAI wire format needs nothing else.
- **Router routes** at `/mm/v1`: everything else the phone's ModelMistress app does: chats and their history, the chat's
  sandboxes, Misty's tools and ratings, the models on the phone and the desktops, downloads from Hugging Face, the benchmark,
  and the owner's settings.

The server, the models and the tool policies are the same code the ModelMistress app runs. The router adds this API; it does
not run a second copy of anything.

## Connecting

- Base address: `http://127.0.0.1:<port>` for apps on the phone (the default port is 8080). The owner can allow other devices
  on the network with the switch on the router's screen. The OpenAI routes are under `<origin>/v1`, the router API under
  `<origin>/mm/v1`.
- Every request except health sends the owner's key: `Authorization: Bearer <key>`. The key is created on first use and can be
  replaced on the router's screen. Health (`GET /v1/health`) needs no key.
- Send `X-MM-Client: <your app name>` on every request. The owner sees this name when your request asks for an approval.
- Pairing: an app that does not have the key opens the router's pairing screen (below). The owner allows it, and the router
  returns the origin and the key.

Errors use the OpenAI shape:

```json
{"error": {"message": "There is no chat 'x' on this device.", "type": "invalid_request_error", "code": "chat_not_found"}}
```

`type` is `server_error` for 5xx and `invalid_request_error` for the rest. Match on `code`.

## The owner's approval

Anything that changes the phone waits for the owner. The router's screen lists each request under "Waiting for you" with
Allow and Deny. Those are the calls that need it:

| Call | Approval text (what the owner reads) |
|---|---|
| `POST /mm/v1/models/default` | Make *id* the default model? |
| `DELETE /mm/v1/models/{id}` | Delete the model from this device? |
| `POST /mm/v1/downloads` | Download *file* from *repo*? |
| `POST /mm/v1/sandboxes` | Make a sandbox called *name*? |
| `POST /mm/v1/sandboxes/{id}/files` | Save *name* in sandbox *id*? |
| `DELETE /mm/v1/sandboxes/{id}/files/{name}` | Delete *name* from sandbox *id*? |
| `DELETE /mm/v1/chats/{id}` | Delete the chat *id*? |
| `DELETE /mm/v1/feedback` | Forget every rating of Misty's replies? |
| `GET /mm/v1/training` | Export Misty's training examples? |
| `POST /mm/v1/benchmark` | Run the device benchmark? |
| A chat tool that changes a file | Set by the tool; the owner's File writes setting decides whether it asks at all |

The call **blocks until the owner answers**, up to 120 seconds (`approvals.timeout_seconds` in the capabilities). A refusal, or
no answer in time, returns `403` with code `owner_declined`. Use a client timeout of at least 130 seconds for these calls. The
API never answers an approval itself; only the owner does, on the router's screen. The router's screen must be open for the
owner to see a request.

Reads never ask: capabilities, settings, device, approvals (the waiting list), models, downloads, chats, sandboxes and their
files, Hugging Face file lists, and the benchmark report.

## Capabilities

`GET /mm/v1/capabilities` tells a client what this router can do right now. Read it before relying on a feature.

```json
{
  "api": "mm/1",
  "version": "0.1.0",
  "chat": {
    "streaming": true, "history": true, "sandboxes": true, "misty_model": "misty",
    "tools": {
      "owner_tool_use": "ON", "owner_file_writes": "ON",
      "names": ["datetime","calc","fs_list","fs_read","fs_search","fs_write","fs_edit","device_status","models_list"],
      "default_for_plain_requests": false, "default_for_chats_and_misty": true
    }
  },
  "misty": {"enabled": false, "reflex": true, "ratings": true, "training_export": true},
  "images": {"available": false, "endpoint": "/v1/images/generations"},
  "models": {"list": true, "default": true, "delete": true, "download": true, "desktops": true},
  "benchmark": true,
  "attachments": {"text": true, "image": false, "document": false},
  "approvals": {"timeout_seconds": 120, "pending": "/mm/v1/approvals", "note": "..."},
  "not_available_over_api": ["speech", "live", "camera", "steer", "queue"]
}
```

`images.available` is set when the server starts: it is true when the phone has an image model at that moment. Restart the
server after adding one.

## Chat

The OpenAI route takes one extension, `mm`, and the model name `misty` for Misty.

```http
POST /v1/chat/completions
Authorization: Bearer <key>
X-MM-Client: MobileAgents

{"model": "misty", "stream": true,
 "messages": [{"role": "user", "content": "What is on my phone?"}],
 "mm": {"chat": "notes-42", "sandbox": "work", "tools": true}}
```

| Request | Answered by | Tools | History |
|---|---|---|---|
| no `mm`, a model name | the chat model, as on `/v1` | none | none |
| `model: "misty"` or `"misty:<model>"` | Misty, on the chat model | yes | only with `mm.chat` |
| `mm.chat` set | the chat model, or Misty when the owner has her on | yes | saved after each reply |
| `mm.tools: false` | either | none | either |
| `mm.tools: true` on a plain request | the chat model | yes, in the default sandbox | none |

- `mm.chat`: 1 to 64 letters, digits, `.`, `_` or `-`. The chat is created on its first reply and its history is kept on the
  phone. A bad id is `400`.
- `mm.sandbox`: an existing sandbox id. An unknown one is `404 sandbox_not_found`. Without it, a chat uses its own sandbox, made
  on first use, and a plain request uses the default one.
- `model: "misty"` while the owner has Misty off is `409 misty_disabled`.
- A client's own `tools` definitions are refused with `400 tools_unsupported`. Router tools come from the router, under the
  owner's policy.

The owner's **Tool use** and **File writes** settings (each Off, On, or Full auto) decide what runs. A write that the owner has
to approve waits as above, and a refused one is reported in the reply as not done.

### Streamed replies

Text arrives as OpenAI chunks, `choices[0].delta.content`. Router events arrive in the same chunk under `mm_event`:

```text
data: {"choices":[{"index":0,"delta":{"mm_event":{"type":"tool_started","name":"fs_read"}}}], ...}
data: {"choices":[{"index":0,"delta":{"mm_event":{"type":"tool_finished","name":"fs_read","outcome":"ran"}}}], ...}
data: {"choices":[{"index":0,"delta":{"mm_event":{"type":"note","text":"Misty chose the calculator."}}}], ...}
data: {"choices":[{"index":0,"delta":{"mm_event":{"type":"replaced","text":"Saved the file."}}}], ...}
data: {"choices":[{"index":0,"delta":{"content":"..."}}], ...}
data: [DONE]
```

- `tool_finished.outcome`: `ran`, `refused` (the owner said no, or the policy denies it), or `failed`.
- `replaced`: the text streamed so far was replaced, because it claimed a file change that no tool made. Use the `text` in
  place of what you showed.

### Whole replies

The non-streamed reply is a normal completion with one extra top-level key:

```json
"mm": {"chat": "notes-42", "misty": true, "tool_calls": 1, "offered": ["fs_read", "datetime"], "calls": ["fs_read:ran"]}
```

`usage` is zeros on this path; token counts are not reported for chats that go through the agent yet.

### Rating a reply

`POST /mm/v1/feedback` rates the latest reply in a chat:

```json
{"chat": "notes-42", "label": "needs_tools", "tools": ["fs_read"]}
```

`label` is `good`, `bad`, `no_tools` (no tool was needed), `needs_tools` (`tools` says which were needed), or `corrected`
(`correction` holds the right answer). Responds `404 no_reply` when the chat has no reply yet.

## Models

| Method and path | What it does |
|---|---|
| `GET /mm/v1/models` | Every model this phone answers with, local (`kind: local`) and on paired desktops (`kind: desktop`). |
| `POST /mm/v1/models/default` `{"id": "..."}` | Makes a local model the default. Approval. |
| `DELETE /mm/v1/models/{id}` | Removes a local model. Approval. The default is cleared if it was that model. |
| `GET /mm/v1/hf/files?repo=owner/name` | The single-file GGUF models in a Hugging Face repo, by path. |
| `GET /mm/v1/downloads` | The current or latest download: `idle` is not listed, otherwise `downloading`, `done`, `stopped` or `failed`. |
| `POST /mm/v1/downloads` `{"repo": "...", "file": "..."}` | Starts the download. `202` with the download. `409 download_busy` while one runs. Approval. |
| `POST /mm/v1/downloads/stop` | Stops the running download. What was fetched is kept for the next start. |

A download's file name comes from `hf/files`. The model is saved to the phone's library and checked by size and SHA-256.

## Device and settings

- `GET /mm/v1/settings`: `tool_use`, `file_writes`, `misty_enabled`, `misty_reflex`, `default_model`, `allow_network`, `port`.
  The key is never included.
- `GET /mm/v1/device`: `total_memory_bytes`, `free_memory_bytes`, `needle_ready`, `loaded_model`.
- `GET /mm/v1/benchmark`: the latest report as JSON, or `404 no_benchmark`.
- `POST /mm/v1/benchmark`: runs the device benchmark in the background, the stress test and each local model's speed.
  `202` when started, `409 benchmark_busy` when it is already running. Approval.

Settings changes stay on the router's screen. The API reads them and does not change them.

## Sandboxes and files

A sandbox is a folder on the phone that chats' tools can use, and that no other app can reach directly.

| Method and path | What it does |
|---|---|
| `GET /mm/v1/sandboxes` | Each sandbox's `id`, `name`, `created_at`, and `links`. |
| `POST /mm/v1/sandboxes` `{"name": "..."}` | Makes one. `201`. Approval. |
| `GET /mm/v1/sandboxes/{id}/files` | The files in it, with sizes. |
| `POST /mm/v1/sandboxes/{id}/files` `{"name": "...", "content_base64": "..."}` | Saves a file, at most 1 MB. `201`. Approval. |
| `DELETE /mm/v1/sandboxes/{id}/files/{name}` | Removes a file. Approval. |

## Chats

| Method and path | What it does |
|---|---|
| `GET /mm/v1/chats` | Each chat's `id`, `title`, `model`, `updated_at`, `messages` count, and `sandbox`. |
| `GET /mm/v1/chats/{id}` | The chat with its messages, as `role` and `content`. |
| `DELETE /mm/v1/chats/{id}` | Deletes the chat. Approval. |

## Misty's training data

`GET /mm/v1/training` returns Misty's examples (accepted replies and corrections) as JSON lines, `application/x-ndjson`.
Approval.

`DELETE /mm/v1/feedback` forgets every rating. Approval.

## Pairing from another app

An app that has no key starts the router's pairing screen for a result:

```kotlin
val intent = Intent("com.modelmistress.router.action.PAIR")
startActivityForResult(intent, REQUEST_PAIR) // the router's screen names the calling app to the owner
```

On **Allow** the result carries:

| Extra | Value |
|---|---|
| `com.modelmistress.router.extra.BASE_URL` | the origin, such as `http://127.0.0.1:8080` (add `/v1` or `/mm/v1`) |
| `com.modelmistress.router.extra.API_KEY` | the owner's bearer key |

On **Deny**, or when the owner backs out, the result is `RESULT_CANCELED`. While the router's server is not running, the screen
says so and Allow stays disabled, so start the server first.

The key stays the owner's. Store it the way the app stores its other secrets, and send it only to the origin the router gave.

## Known limits

- Not run on a phone yet. The host tests cover the server side (`server:test`, including the router routes against a fake
  device); the device side (`RouterHost`) is compiled but not exercised on a phone.
- Over the API: no speech, Live, camera, steering or queueing. Attachments are text only; images and documents are not
  accepted yet.
- Usage counts are zero on the chat route through the agent.
- `images.available` is set at server start.
- An approval appears only while the router's screen is open. The request times out after 120 seconds and is refused.
- The owner's screen does not browse chats or sandboxes yet; the API serves them.
- The router's screen does not export Misty's training file; the API route does, once approved.
