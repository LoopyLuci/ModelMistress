---
name: model-mistress
description: Use local language models served by ModelMistress on this machine, when the user asks for a local, private or offline model, or when text should not leave the machine. Covers listing, loading, unloading and chatting with models through the model-mistress MCP tools.
---

# Using ModelMistress from Claude

ModelMistress serves local models (GGUF files run by its built-in llama.cpp) on this machine. Its MCP server exposes
every hub operation as a tool. The tools you will use most:

- `model_list`: the models on this machine, with their source, size and whether each is loaded.
- `model_load` / `model_unload`: put a model into memory or free it. Loading can take minutes and a lot of GPU or RAM
  memory. Unload models you no longer need.
- `chat_complete`: one prompt in, one answer out, with a model named by `model`.
- `service_status`: the hub's version, memory and loaded models. Start here when something does not respond.

Guidance:

- Check `model_list` before a chat so you name a model that exists. Pick a chat model, not an embedding or reranking one.
- Load a model only for the work at hand, and unload it afterwards when memory is tight.
- Local answers are private and cost nothing per token, but they are usually weaker than a hosted model. Say so when
  you hand over a local answer.
- If the server is missing, the `model-mistress` program is not on the PATH or the hub is not running. The server is
  started with `--start`, which starts a hub in the background when none answers.
