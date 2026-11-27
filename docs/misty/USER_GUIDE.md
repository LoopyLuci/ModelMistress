# Misty: user guide

Misty is an agent that lives inside the ModelMistress hub. You give her a request; she picks the smallest model that can
answer it, searches your indexed documents when that helps, uses tools under a policy you control, and answers. She is
built for a CPU: the small models do most of the work and the large ones are used only when a request needs them.

This guide is for using her. [DESIGN.md](DESIGN.md) says why she is built this way, and [API.md](API.md) is the reference
for programs. [STATUS](#what-is-tested-and-what-is-not) at the end says what has been run and what has not.

## Start

You need a running ModelMistress build (`model-mistress serve`, or the desktop app). Then:

```bash
model-mistress misty setup
```

This prints what can be installed, how big each piece is, and what is already here. It downloads nothing. To install the
starter set:

```bash
model-mistress misty setup --install default
```

The starter set is the Prism engine (a llama.cpp build that reads every ternary Bonsai packing; the ModelMistress build
bundles a llama.cpp that cannot yet; on a machine where a GPU is found, also its Vulkan build, `prism-gpu`), the Cactus runtime, the Needle3, Whistle and Mirage models, and Bonsai 2B and 9B. You can name pieces instead: `--install prism,bonsai-4b`. Every
download is checked against a pinned size and SHA-256 before it is used, and runs as a job you can follow on the
dashboard's Models and Misty setup pages.

You do not need all of it. Any chat model already in your ModelMistress catalog (a Qwen, a Llama) can fill Misty's
roles; the starter set is what she prefers. With nothing installed she says so and tells you what to do:

```bash
model-mistress misty status      # who fills which role, what is missing, what to do next
model-mistress misty doctor      # checks everything and makes one real call of each kind
```

## Talk to her

| Where | How |
| --- | --- |
| ModelMistress CLI | `model-mistress misty chat` (talk; `/help` inside), or `model-mistress misty ask "your question"` |
| ModelMistress dashboard and desktop app | the **Misty** page: chat, sessions, approvals, where she works, and **Environments** and **Setup and status** |
| ModelMistress Windows/Qt window | the **Misty** page |
| NLP-CLI | `nlp-cli misty ask "your question"`, `/misty` in its shell and windows, and automatically for a sentence it cannot turn into a command. Read-only: every request is sent with approvals denied. Described in NLP-CLI's `docs/MISTY.md` |
| ABP CLI | `abp misty ask "your question"`, `abp misty status`, `abp misty search`, `abp misty ingest`, `abp misty setup`, `abp misty doctor` |
| ABP agents | the tools `misty_ask`, `misty_search`, `misty_ingest`, `misty_status` |
| ABP's generic module panel | should list the `misty.*` operations as forms (ABP's module contract; not opened in a running ABP) |
| Any OpenAI client | the model `misty` (also `misty-fast`, `misty-main`, `misty-deep`) at the hub's `/v1` |
| MCP | the hub's MCP server (`model-mistress mcp`) lists every hub operation as a tool, `misty.*` included |

The ABP path is deliberately narrower than the others: ABP's `ask` is read-only, because ABP has its own approvals for
what an ABP agent may do. A tool of Misty's that needs your approval is refused on that path. Use her own surfaces when
you want her to write a file.

### What she does with a request

1. **Routes it.** A router scores the request (its length, whether it mentions code, files, documents, steps or a
   tool, whether your index matches it) and picks a role: `fast`, `main` or `deep`. `model-mistress misty route "..."`
   shows the scores and the reasons without running anything. Writing `@fast`, `@main` or `@deep` in the text, or
   `--role`, overrides it.
2. **Retrieves.** When the router thinks your documents may help, or you ask for it (`--rag`), the best passages are
   put into the prompt, marked as data, and cited as `[1]`, `[2]` in the answer. The sources listed under an answer are
   the passages it actually cites; what was retrieved is in the run's `retrieve` event.
3. **Tries Needle first.** For a clear tool request ("list the files in notes"), Needle3 can choose the tool call
   without a language model. It runs on its own only when the tool just reads and Needle's confidence is at least
   `reflex_threshold` (0.10, the lowest score at which Needle gives a call at all). If it is not sure, or the call would
   change something, a model decides. After a call Needle chose, the small `fast` model puts the result into words
   (unless you named a role with `@main` or `--role`), which on a CPU is several times quicker than the 9B. A request
   that is plainly about one kind of thing (files, documents, the time, a sum, models, audio, an image) is offered only
   those tools: every tool definition is text the model has to read first.
4. **Generates**, calling tools as needed. Each tool call is checked against the policy below.
5. **Checks itself once.** If the answer is empty, a refusal template, a repetition loop or a thinking run that never
   finished, she retries one role up and records why. Never more than once per request.
6. **Remembers** the exchange in the session, so `--session ID` continues it.

### Sessions

Every `ask` can continue a session (`--session ID`) or start one (`--new-session`). `model-mistress misty sessions`
lists them, `sessions show ID` prints one, `sessions delete ID` removes it.

### Earlier messages

In the message box of the **Chat** page and of the **Misty** page, **Up** brings back the message you sent before and
**Down** walks forward again; past the newest it gives back what you were typing. What Up walks through is, oldest
first, what you sent in other chats (kept in this browser, the last 200) and then what you sent in this one, so a new
chat can repeat or fix the last thing you sent anywhere. In a message of several lines Up and Down move the cursor as
usual, and only on the first line (Up) or the last (Down) do they walk the history. Typing something walks you back out
of it. Ctrl+Enter sends from the Misty box, as in Chat.

## Ratings, and what she learns

Under each reply on the **Misty** page are the ratings: **Good**, **Bad**, **No tool needed**, **Should have used a
tool**, and **Correct it...**, which opens a box for the right answer. A rated reply shows its rating. Each rating is kept
on this machine with the question, the reply, the tools that were offered for it, and the time. **Should have used a
tool** is refused for a reply that was offered no tools, because there is no tool for it to point at.

What she does with them:

- **No tool needed** and **Should have used a tool** change routing for similar questions. After "No tool needed" she
  offers no tools. After "Should have used a tool" or "Good" she offers the tools that were offered on the rated reply.
  A question is similar when at least 60% of the distinct meaningful words the two share (common words and single
  letters are ignored). The newest matching rating wins.
- **Correct it...** does not change routing. When a similar question comes up, the two most similar corrections are
  given to her as a note: the earlier question, the wrong reply (its first 300 characters), and the correct answer. She
  is told to follow the correction and not to repeat the wrong reply.
- A request that names its tools (`tools` or `no_tools` in the API) overrides the ratings.

She keeps the newest 500 ratings, and a stored reply is cut at 1000 characters.

**Learning tab.** On **Setup and status**, the **Learning** tab shows how many ratings she holds. It exports them as
chat-format JSON lines (`misty-training.jsonl`), and it forgets one rating or all of them after a confirmation. The export
holds the **Good** replies (the question and the reply) and the **Corrected** replies (the question, with the correction
as the answer and the wrong reply in `metadata.rejected`). The other ratings are kept but not exported.

## Documents

Misty keeps her own index (SQLite, keyword search, plus vectors when an embedding model is installed):

```bash
model-mistress misty rag add D:\notes            # a file or a folder; unchanged files are skipped on the next run
model-mistress misty rag search "release date"   # the passages that match; no model runs
model-mistress misty rag sources                 # what is indexed
model-mistress misty rag remove notes/old.md     # one source; --all removes a collection
```

Files that look like secrets (`.env`, private keys, credentials) are never indexed. `rag add` is you asking, so it takes
any folder you name (use a full path, or one relative to where you ran the command; a path that is not there is an
error). When Misty adds something herself through her `rag_ingest` tool, it must be inside the folders she is allowed
to use (see **Folders**) and you are asked first. Use `--collection NAME` to keep separate indexes; the default is
`default`.

### An embedding model

Without one, search is keyword only. With one, each passage also gets a vector and a question finds passages by
meaning. Any embedding model in the catalog is used (a name containing `embed`, `bge`, `e5-`, `gte-` or `nomic`):

```bash
model-mistress pull nomic-embed-text          # 274 MB
model-mistress misty status                   # embed: ready
model-mistress misty rag add D:\notes         # embeds what is not embedded yet; run it again to carry on after a stop
```

`rag add` prints `embedded` (passages given a vector) and, if the embedder failed, `embed_error` with the reason. The
passages still work by keyword. A passage too long for the embedder's batch is cut to fit before it is embedded.

**On the bundled llama.cpp, `nomic-embed-text` can crash** (Windows exit code `0xC0000409`): when it is offloaded to the
GPU, and when its batch is above 512. The Prism engine, which `misty setup --install prism` installs, does not. Give the
embedder a profile that uses it, on the CPU, in `config.toml` (restart the hub after editing):

```toml
[profiles."nomic-embed-text:latest"]
engine = "prism"
gpu_layers = 0
ctx_size = 2048
batch_size = 2048
ubatch_size = 2048
```

On a CPU, expect about 1,000 tokens a second: a thousand passages take a few minutes. The first `rag add` of a large
folder is the slow one.

## Tools, approvals and policy

Misty can use these tools. Each belongs to a class, and the policy decides what happens when it is called.

| Class | Tools | Default |
| --- | --- | --- |
| `read` | `fs_list`, `fs_read`, `fs_search`, `rag_search`, `rag_sources`, `datetime`, `calc`, `transcribe_audio`, `describe_image` | **auto**: runs at once |
| `write` | `fs_write`, `rag_ingest` | **ask**: the run waits for you |
| `exec` | `shell_run` | **deny** |
| `net` | `web_fetch` | **deny** |
| `hub` | `hub_status`, `models_list`, `model_load`, `model_unload` | **ask** |

```bash
model-mistress misty tools              # every tool, its class and the policy that applies
model-mistress misty policy             # the policy
model-mistress misty policy write auto  # change one class: auto | ask | deny
model-mistress misty approvals          # tool calls waiting for you
model-mistress misty approve ID         # or: deny ID
```

When a tool needs approval, `chat` and `ask` ask you in the terminal; the dashboard and the Qt window show a prompt
with the tool's arguments. A run that waits longer than `approval_timeout_s` (300 s) counts the silence as no.

What she reads is treated as data. A file, a web page or a document may contain instructions; her tools wrap what they
return so the model is told it is untrusted, and an instruction found there does not change what she is allowed to do.

### Folders

The file tools reach only the folders in `roots` (default: `<home>/misty/workspace`). Nothing outside them, ever, and
secret-looking files inside them are refused. Add a folder:

```bash
model-mistress misty config set '{"roots": ["D:/notes", "D:/projects/docs"]}'
```

### Environments and working folders

Left alone, she works in her own workspace folder under the policy in her settings. An **environment** is a named setup
you make once and choose for a chat instead:

- a **working folder**: a file name without a folder means this folder, and her commands start in it;
- **other folders** her file tools may also reach;
- **variables** her commands can see (a command otherwise gets almost nothing from your environment);
- a **policy** for each class of tool (read, write, commands, network, models): *Allow without asking*, *Ask me each
  time*, *Never*, or *Use Misty's setting*, which keeps the policy from her settings.

In the dashboard and the desktop app, **Misty, Environments** lists, makes, changes, duplicates and deletes them, with
**Browse...** to pick a folder, and **Use in a new Misty chat** starts the next chat in one. The bar above the chat on the
**Misty** page chooses the environment and the working folder of the chat that is open; a new chat takes them with its
first message and starts where the last one worked. **Change...** gives the chat a folder of its own (the folder list
shows the machine the hub runs on, so it is right even when that is another machine), and **Use the environment's** (or
**Use Misty's own**) takes it away. The line under the bar says what each class of tool does in that chat. Setting
commands or the network to *Allow without asking* asks you to confirm first.

The chat's own folder wins over the environment's; with neither, her own workspace applies. Changes apply from the next
tool call, also to a run that is going. Deleting an environment leaves the chats that used it with their own folder, back
under her policy. Only you can make or change an environment: she has no tool for it.

```bash
model-mistress misty env                       # the environments
model-mistress misty env delete my-app
model-mistress misty ask "what changed?" --new-session --env my-app --cwd D:/projects/app
model-mistress misty chat --env my-app         # a session remembers its environment and folder
model-mistress misty sessions set ID --env my-app --cwd D:/projects/app
model-mistress misty sessions set ID --env none --cwd none
model-mistress call misty.env.save '{"name": "my-app", "cwd": "D:/projects/app", "vars": {"MODE": "dev"}, "policy": {"exec": "ask"}}'
```

Variables are plain text on the machine the hub runs on and a command can print them: put in nothing she must not see.
The secret and escape guards of the file tools hold inside every folder.

## Models and engines

Misty fills each role from your catalog: first the model you named in `roles`, then the first installed model whose name
matches the role's preferred names (the table), then, for the three language-model roles, any installed chat model of a
suitable size that fits free memory:

| Role | What it is for | Preferred |
| --- | --- | --- |
| `reflex` | choosing a tool call without a language model | Needle3 (Cactus) |
| `fast` | short answers, chat | Bonsai 2 2B, then 4B, then Qwen3.5 2B |
| `main` | most requests, code, documents | Bonsai 2 9B, then 4B, then Qwen3.5 9B |
| `deep` | hard reasoning (thinks first); unused unless installed | Bonsai 2 27B, then Qwen3.5 27B |
| `ear` | speech to text | Whistle (Cactus) |
| `eye` | describing images | Mirage (Cactus) |
| `embed`, `rerank` | retrieval | an embedding model in your catalog, if any |

"MoE" here means a **mixture of models**: the router sends each request to a different model. Bonsai 2 itself is a dense
model, not a mixture of experts. A panel mode (two roles propose, a third aggregates) exists for hard requests; it is
off by default because it is slow on a CPU (`"panel": true` to allow it).

The ternary Bonsai files need an engine that can read them. Stock llama.cpp at the version ModelMistress bundles
cannot; the **Prism** engine (`setup --install prism`) can, and ModelMistress uses it for those files automatically. A
ternary file with no engine that can run it is refused with a clear message (HTTP 409, `engine_cannot_run`) instead of
failing during load. `misty status` shows which engines are present.

The Prism engine installed by `--install prism` runs on the CPU. On a machine with a Vulkan GPU, `misty setup --install
prism-gpu` adds the same build with the Vulkan backend (29 MB, pinned like the rest; the starter set takes it when a GPU is found).
ModelMistress then runs the `main` and `deep` ternary models on the card, as far as the card has room for them (what is
left after `vram_reserve_mib`), and keeps `fast`, the embedder and the reranker on the CPU, so that the two processors
work at the same time. On an RX 7900 XTX a 2B `PQ2_0` file read a prompt 15 times faster and generated 2.2 times faster
on the card, with the same answers. A ternary file that does not fit the card, or a load the card cannot take, runs on the
CPU build. A model whose profile sets `gpu_layers` is left where the profile puts it.

A model that does not fit in free memory minus `ram_margin_mib` (2 GiB) is marked `unfit` in `misty status` and not
used for that role; she falls back to a smaller one when there is one.

## The model router: `auto`, in any chat

Her router is also available without her. The model name **`auto`** is a plain chat that goes to the model that suits
each message: short, simple turns to the `fast` model, most requests to `main`, hard reasoning and long inputs to `deep`.
A model that is already loaded is preferred, so it does not load a bigger one for a greeting. `auto` has no tools, files
or index; for those use `misty`.

Where you can use it:

- **Chat page** (dashboard and desktop app) and the **Qt window**: choose "Auto: the router picks the model for each
  message", the first entry of the model list. It is the default for a new chat once a role has a model that can answer.
  Each reply says which role and model answered and, under "Why the router chose...", what decided it.
- **Any OpenAI client**: `model: "auto"` on `/v1/chat/completions`. The answer's headers say what happened:
  `x-mm-routed-model`, `x-mm-routed-role` and `x-mm-routed-why`.
- **Your own choice for one message**: put `@fast`, `@main` or `@deep` in it. The tag is removed before the model sees
  the message.

A conversation with a picture goes only to a model that has a projector (`mmproj`). If none does, the answer is
409 `no_model_ready` with the reason (the chat page shows it); the same when no role has a model that is installed,
fits the free memory and has its engine.

**Choosing the models.** Open the Models page, **Router** tab (the chat's "Choose models" link goes there). Each role
has a menu: Automatic (a model the router prefers by name, or an installed chat model of the right size that fits the
free memory) or any suitable installed model. The same choice is the `roles` setting of `misty config` (it is one
setting, so the dashboard, the Qt window and the CLI agree). The embedding and reranking models are chosen in the same
table. **Try the router** shows where a message would go and the score of each role, without sending it.

**Managing models** (Models page, Installed tab):

- Each model shows its router roles; the "Use for" menu gives it one.
- **Settings** edits that model's serving profile (`[profiles."<id>"]` in `config.toml`): engine, GPU layers, context
  size, parallel requests, batch sizes, flash attention, KV cache types, prompt cache, Jinja. The hub checks the values
  and says what is wrong. A profile wins over the global setting, whoever loads the model (a chat, the router, another
  program), and applies the next time the model loads ("Save and reload the model" does that now). "Reset" removes it.
  This is how an embedding model gets the batch sizes and `engine = "prism"` it needs (see An embedding model).
- **Load...** checks the fit as you type (`model.plan`): fits, fits after the hub unloads something, or does not fit,
  with the memory it needs and what is free. The check does not load anything.
- **Delete** works only for models ModelMistress downloaded (the hub refuses files other programs put there, and the
  button says so).
- Filter by name, family or quantisation; show only loaded models, models with a router role, models with custom
  settings or models that can be deleted; sort by name, size or loaded first.

**Peers.** A peer key may use `auto` only if its allow-list names it, and then the router considers only the models
that key may use, so a key limited to one model can never be sent to another. A catalog model that is itself called
`auto` wins over the router.

## How fast is she on a CPU

Measured with `llama-bench` and `misty bench` on one machine: a Ryzen 9 5900X (12 cores, 12 threads used), Windows 10,
the Prism engine (build 12128), the `PQ2_0` Bonsai files. Yours will differ; `model-mistress misty bench` measures it.

| Role | Reads a prompt | Writes an answer |
| --- | --- | --- |
| `fast` (Bonsai 2 2B) | about 190 tokens a second | about 40 tokens a second |
| `main` (Bonsai 2 9B) | about 30 tokens a second (55 on a 64-token prompt) | about 12.6 tokens a second |

The number that matters is the first one for the 9B. A request with a few documents or tool definitions in front of
it is some 600 to 1,600 tokens, which is 20 to 55 seconds of reading before the first word of the answer. That is why
Misty keeps prompts short: she offers only the tools a request is about, puts retrieved passages in under a token budget,
describes pictures to the model in a line instead of Mirage's full result, and lets the `fast` model word the result of
a tool Needle already ran. What you saw on that machine, start to finish: a plain question to `fast` 1.5 s; a sum 3 s;
"what time is it" 7 s; "list the files in notes" 10 s; a question answered from your documents by `main` 9 s; a spoken
question 13 s (Whistle took 0.7 s of it); a question about a picture 26 s (Mirage took 1 s). When the 9B is too slow for
what you are doing, write `@fast` in the request (or `--role fast`).

The 4B sits between them on the same machine: about 70 tokens a second reading, about 20 writing. Used as `main`
(`config set '{"roles": {"main": "prism-ml/Ternary-Bonsai-2-4B-gguf:Ternary-Bonsai-2-4B-PQ2_0"}}'`), it answered the
document question above correctly with its citation in 2.5 s warm, against 8.8 s for the 9B.

Ten questions about an index of 3,503 passages (the docs of three projects), each answered by `main` with retrieval and
scored by whether the answer held the expected terms: the 4B passed 7 in 331 s in all, the 9B passed the same 7 in 951 s
(one answer hit a 10-minute cap). Both missed the same questions (a list of six settings came back with three), which
is retrieval and chunking, not the model. The 9B also takes about 2.4 GB more memory: on a machine with about 7 GB free,
the 9B with the 2B and an embedding model left 2.5 GB, and the 4B was then refused as unfit. Misty prefers the 9B for
`main`; if you are short of memory or patience, pin the 4B as above. `config set '{"roles": {"main": null}}'` goes back
to her own choice.

## Pipelines

A pipeline is a short, ordered recipe of steps; each step can use what earlier ones produced:

```toml
# <home>/misty/pipelines/voice-note.toml
name = "voice-note"
description = "Transcribe a recording, summarize it, file the transcript in the index"
inputs = ["path"]
output = "{{steps.summary.text}}"

[[steps]]
id = "heard"
kind = "transcribe"
path = "{{input.path}}"

[[steps]]
id = "summary"
kind = "ask"
prompt = "Summarize in three bullet points:\n{{steps.heard.text}}"
```

Step kinds: `ask`, `route`, `rag.search`, `rag.ingest`, `transcribe`, `describe`, `extract`, `tool`, `map`, `moa`. It is
a sequence, not a graph: there is no branching. A tool step goes through the same policy as any tool call. Built in:
`voice-note`, `describe-image`, `repo-qa` and `research-brief` (that last one needs `net` allowed).

```bash
model-mistress misty pipeline list
model-mistress misty pipeline show voice-note
model-mistress misty pipeline run voice-note --input path=D:\recordings\a.wav
model-mistress misty pipeline save my.toml        # checked before it is saved
```

## Settings

`model-mistress misty config` shows all of them; `config set '<json patch>'` changes some (checked, then saved to
`<home>/misty/config.toml`). A patch is merged into what is there, and `null` takes a setting away (back to its default):
`config set '{"roles": {"main": null}}'` drops one role override, `'{"roles": null}'` all of them.

| Setting | Default | Meaning |
| --- | --- | --- |
| `roots` | the workspace folder | where the file tools may go |
| `policy` | read auto, write ask, exec deny, net deny, hub ask | see above |
| `max_steps` | 8 | model calls plus tool rounds in one run |
| `max_tokens` | 2048 | output tokens per model call |
| `run_timeout_s` | 900 | a run is stopped after this long |
| `approval_timeout_s` | 300 | silence on an approval counts as no |
| `escalate` | true | retry one role up on a weak answer (once) |
| `panel` | false | allow the panel for hard requests |
| `reflex`, `reflex_threshold` | true, 0.10 | let Needle run a clear tool call that only reads (raise the threshold to be stricter) |
| `think_roles` | `["deep"]` | roles that think before answering (costly on a CPU) |
| `rag_top_k`, `rag_budget_tokens` | 6, 1500 | passages retrieved, and how much text goes in the prompt |
| `collection` | `default` | the index used when none is named |
| `roles` | empty | force a model for a role: `{"main": "qwen3-8b"}` |
| `persona` | none | replace her built-in system prompt |
| `threads` | 0 | Cactus CPU threads (0: all the process may use) |
| `ram_margin_mib` | 2048 | memory that must stay free after a model loads |

## Peers

A peer key (see [hosting](../hosting.md)) reaches Misty only through the OpenAI routes, and only when the key's allow-list
names her (`misty`, `misty-fast`...). A key with no list does not get her along with every model. A peer gets her
answer and nothing else: no tools, no files, no index. Her operations (`misty.*`) are the owner's alone.

## If something goes wrong

| You see | Do |
| --- | --- |
| `no language model is ready` | `misty setup`, or load any chat model in the dashboard's Models page |
| `engine_cannot_run` (409) for a Bonsai file | `misty setup --install prism` |
| Needle, Whistle or Mirage "not installed" | `misty setup --install cactus,needle3,whistle,mirage`, then `misty doctor` |
| She is slow | `misty bench` times each ready role; a smaller `fast` role or `think_roles: []` helps most |
| She is slow only when the hub runs from a Windows Scheduled Task | The task starts the hub at below-normal priority and its model servers inherit that, so other programs take the CPU first (the same question took 11.7 s that way and 2.5 s at normal priority). Set `raise_priority = true` in the hub's `config.toml`, or switch it on in the dashboard's Settings page, and restart the hub |
| A tool was refused | `misty policy` shows why; the refusal names the class and the folder |
| The answer cites nothing you expected | `misty rag search "..."` shows what the index holds for that wording |
| `rag add` says `embedded: 0`, or has an `embed_error` | The embedder did not run; the error says why. See **An embedding model** above |

## What is tested, and what is not

Tested with unit and integration tests (scripted models, no real ones): the router, the agent loop (tools, approvals,
retries, citations, cancellation), retrieval, the policy and file/secret/SSRF guards, pipelines, the REST, SSE and
OpenAI-virtual-model surfaces, peer rules, the setup planner (against pinned metadata), the ternary-engine guard, the
CLI, the ABP client, tools and routes, environments and working folders (the policy, folder and variables a session
gets from its environment, the folder browser), the ratings and what she learns from them (the store, the operations, the export), and the Qt page, including its queue, steering, ratings and "What she learns" dialog.

**Run for real** (Windows 10, Ryzen 9 5900X, 2026-11-21, a separate hub and home): the starter set was installed with its
size and SHA-256 checks; `misty doctor` passed; Bonsai 2B and 9B ran on the Prism engine; Needle3, Whistle and Mirage ran
on the Cactus library; and a document question with a citation, tool calls, a refused write, a spoken question, a
picture question, the `voice-note` pipeline, the OpenAI route and ABP's client all worked. The 4B was measured and used
once as `main`.

**Not run:** the 27B (`deep`) and so the escalation up to it and its thinking mode; the `Q2_0` and `PTQ1_0` packings;
other operating systems and CPUs; the Misty page of the dashboard and desktop app (type-checked, unit-tested and built,
and the desktop app attaches to a running hub, but the page was opened only in a browser, against a scratch hub: the environment bar, the folder picker, the
Environments page and Up/Down history on 2026-11-23, not in the desktop app, and no real model ran in an environment) and of the Qt window (its pages and
the Live, queue, steer and ratings paths run offscreen against a fake hub, in `gui/tests`; not clicked through on a screen, and no real microphone or camera); ABP's dashboard and CLI (only the client module under them ran). The ratings, the Learning tab and the export were not clicked through in the desktop app, and the export's file download was not checked in the desktop app's WebView2.

**Known rough edges.** Needle chose the right tool for 15 of 21 test requests among fifteen tools and called none of
the six requests that needed none; it is a shortcut for clear requests, not the decider. Mirage described a plain grey
square as repeated words, but read a picture of shapes and a word correctly. Whistle misspelled one name in synthesized
speech. The 9B is slow to read long prompts (above). A small model sometimes explains a refused action badly: the refusal
itself (`denied`, in the run's events) is what to trust.
