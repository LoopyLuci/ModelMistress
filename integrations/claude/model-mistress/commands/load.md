---
description: Load a model into memory with ModelMistress
argument-hint: <model id>
---

Load the model `$ARGUMENTS` with the `model_load` tool of the model-mistress MCP server, passing it as `model`. A load
can take minutes and a lot of memory, so say that before you start. When it finishes, report the model that is loaded
and the memory it uses. If no model was given, call `model_list` and ask which one to load.
