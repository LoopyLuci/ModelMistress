---
description: What ModelMistress offers through this plugin, and the command line
---

Explain briefly what this plugin gives Claude and how to use it:

- Slash commands: `/model-mistress:status` (hub state), `/model-mistress:models` (what is on this machine),
  `/model-mistress:load <model>`, `/model-mistress:unload <model or *>`, `/model-mistress:chat <model> <question>`,
  and `/model-mistress:help` (this).
- MCP tools from the model-mistress server: `model_list`, `model_load`, `model_unload`, `chat_complete`,
  `service_status`, and every other hub operation (`model_search`, `model_pull`, `job_get`, ...), listed by the server.
- The command line: `model-mistress help` lists its commands, and `model-mistress help <command>` explains one.

Keep it short, and say that the server needs the `model-mistress` program on the PATH.
