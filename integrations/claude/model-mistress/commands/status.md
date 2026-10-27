---
description: Show the ModelMistress hub's version, memory and loaded models
---

Call the `service_status` tool of the model-mistress MCP server. Summarize the answer in a few lines: the hub's version,
its memory (free and used by the hub), the models loaded now, and any settings warnings. If the tool reports that the
hub is not running, say so and suggest `model-mistress serve` (or starting the MCP server with `--start`).
