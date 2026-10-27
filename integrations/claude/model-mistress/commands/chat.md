---
description: Ask a question of a local model through ModelMistress
argument-hint: <model id> <question>
---

The first word of `$ARGUMENTS` is the model id and the rest is the question. Send the question to that model with the
`chat_complete` tool of the model-mistress MCP server, passing the model as `model` and the question as the prompt.
Show the answer as it is, then say which model answered. If the model is not loaded yet, the tool loads it; a load can
take minutes. If no question was given, ask for one.
