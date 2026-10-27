---
description: Unload a model from memory with ModelMistress (or every model with *)
argument-hint: <model id or *>
---

Unload `$ARGUMENTS` with the `model_unload` tool of the model-mistress MCP server, passing it as `model`. Use `*` to
unload every model. Report what was unloaded. If no model was given, call `service_status` and ask which loaded model
to unload.
