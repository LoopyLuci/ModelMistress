# Connect software, phones and chat bots

The **Connect software** tab on Models gives the addresses and snippets for other software, the phones and other
ModelMistress devices a model can be sent to, and the chat bots. Nothing here is answered until you allow someone.

**Status.** The screens and the hub's settings are tested. Not yet tried: a real Telegram or Discord bot, a real transfer
to a phone, and a model answer through a bot. The Discord module is not compiled yet (see the CHANGELOG).

## Other software

Make a key for the software's name. The key is shown once, so copy it then. Choose the model and copy the snippet for the
tool: the OpenAI-compatible `/v1` address, the Ollama-compatible `/api` address, OpenCode, Continue, Hermes Agent, OpenClaw,
or environment variables. A key can chat with the models; it cannot load or delete one. That stays with the owner.

## Phones and other devices

1. On the phone, turn on its network server and note its key.
2. Under **Phones and other devices**, add the device with its address and key, then **Save devices**.
3. **Check** asks the device whether it answers. Check is available once the device is saved.
4. On Models, Installed, open a model's **Send to phone...** and choose the device. The transfer shows at the top of the
   page with its progress, and **Stop** cancels it.

Only `.gguf` files the hub holds are sent. Ollama models, models split into parts, and other files are not offered.

## A Telegram bot

1. In Telegram, message @BotFather, send `/newbot`, and copy the token it gives you.
2. Under **Chat bots**, add a bot, choose the service Telegram, paste the token, and **Check token**.
3. Send the bot a message from the chat you want to allow. It replies with that chat's id. Put the id under
   **Chats allowed** (a group has a negative id), turn the bot **On**, and **Save bots**.
4. `/reset` starts a new conversation. Conversations are kept in memory, so restarting the hub forgets them.

## A Discord bot

1. In the Discord developer portal, create an application, add a bot, and reset its token to copy it.
2. On the bot's page, turn on the **Message Content** intent. Without it a server message has no text.
3. Invite the bot to a server, with the `bot` scope and the permissions to read and send messages.
4. In ModelMistress, add a bot, choose the service Discord, paste the token, and **Check token**.
5. Under **People allowed**, add your own user id (Discord's Developer Mode copies it). Turn the bot **On** and **Save bots**.

A direct message to the bot is answered. In a server, the bot answers only a message that mentions it.

## Where the secrets are

Tokens and keys are kept in the hub's settings on this machine. The settings screen shows them as `(set)`. A token or key
never appears in an error message or in the hub's answers.
