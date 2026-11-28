# Data-broker opt-outs

ModelMistress drives [f*ck_data_brokers](https://github.com/gary23w/f-ck_data_brokers) (MIT): one Python file, no
dependencies, that takes the public data-broker registries, writes each broker a legal deletion and opt-out letter in your
name (CCPA, GDPR, UK GDPR, PIPEDA, Quebec Law 25, LGPD, or a plain policy request), reads their replies, and chases the ones
that go quiet. It never sends an ID, never clicks a broker's link and never gets past a CAPTCHA. Everything it keeps is on this
PC. `src/privacy/` is the integration; the tool itself is upstream's and is not copied, edited or bundled.

## How it is kept safe

- **Pinned.** `privacy install` fetches the one file, `fuck_data_brokers.py`, from
  `https://raw.githubusercontent.com/gary23w/f-ck_data_brokers/528fb023821c1e0f9f9241921e723af25f2235c8/fuck_data_brokers.py`
  (commit `528fb023`, 85,839 bytes) and keeps it only if its SHA-256 is
  `dce00f90cfa888db0116afe68432fdf65bbec28a4239427b4bf20efded2aa093`. A file with any other hash is never written. The hash
  is checked again before every call, so a file changed on disk is refused until `privacy install --force` puts the pinned one
  back. Moving to a newer upstream means changing the three constants in `src/privacy/mod.rs`, which is a review of that
  version.
- **Your yes.** Nothing is downloaded until you ask: `privacy.install` needs `confirm: true` (the CLI shows the URL, size and
  hash and asks). If you would rather fetch the file yourself, `--file PATH` installs your copy, which must match the same hash.
  The tool itself makes network requests when you ask for them: the broker registry (`brokers import`) and, if you send from
  the mailbox, your own mail server.
- **Owner only.** The `privacy.*` operations are the owner's; a peer key reaches the model routes only, and Misty has no privacy
  tool.
- **Nothing is sent without your count.** `letters`, `forms`, `chase` and `routine` write drafts and a send page on disk and
  send nothing. `privacy send` first says how many emails would go, from which address and to whom; it sends only when called
  again with `confirm_count` equal to that number (the CLI asks you to type it). If the queue changed in between, it refuses
  and says what it holds now. An unreadable state file is never guessed at.
- **The password is never an argument.** Sending from the hub needs `FDB_SMTP_PASS` (an *app password* for your mailbox, which
  only you can create) in the hub's own environment. It is handed to the tool for the one call that sends, and is never stored,
  logged or returned. Without it, use the send page: one button per letter that opens it in your own mail program, no
  password, no setup (`privacy status` shows where it is). `FDB_SMTP_HOST`, `FDB_SMTP_PORT` and `FDB_SMTP_USER` are passed
  through for a provider the tool does not know.
- **A clean room.** The tool runs as `python -I -X utf8 fuck_data_brokers.py mcp`, in Python's isolated mode, with an
  environment of only what Python and the network need (system folders, `PATH`, proxy and certificate variables), so none of the
  hub's own tokens or keys reach it, and with one call at a time because the tool keeps its state in a file without locking.
- **Its own folder.** `<home>/privacy/`: `tool/` (the pinned file and `SOURCE.json`) and `state/` (your name, email and city, the
  broker list, the requests, the drafts and the send page).

## Using it

```
model-mistress privacy status
model-mistress privacy install
model-mistress privacy setup --name "Ada Lovelace" --email ada@example.com --city London --country UK --place uk
model-mistress privacy brokers import
model-mistress privacy brokers find acme
model-mistress privacy brokers skip "some broker"        # you decide who gets a letter
model-mistress privacy letters --limit 20
model-mistress privacy send                              # preview, then type the number
model-mistress privacy sent all                          # after using the send page yourself
model-mistress privacy reply --ref req-1a2b3c4d reply.txt
model-mistress privacy chase                             # reminders, and where to complain
```

`place` is `california`, `us_other`, `quebec`, `canada_other`, `eu`, `uk`, `brazil` or `other`: it decides which law the letter
quotes. A sent letter is not a deletion: "done" means the broker said so, and the tool says so too.

Operations: `privacy.status`, `privacy.install`, `privacy.setup`, `privacy.brokers.import|find|add|skip`, `privacy.letters`,
`privacy.forms`, `privacy.form_demands_id`, `privacy.send`, `privacy.mark_sent`, `privacy.reply`, `privacy.chase`,
`privacy.routine`. They pass your arguments to the tool's own MCP tools (as strings, as it expects) and return what it answered;
a refusal from the tool comes back as a 422 `refused` with its own words.

## Limits

- Needs Python 3.8 or later: `MM_PRIVACY_PYTHON`, the Forge's interpreter, then `python`, `python3`, `py -3`. The Microsoft
  Store's stand-in `python` is detected and skipped.
- The reply reader is upstream's pattern matching on a broker's text; it is a help, not a judgement, and the replies it reads
  are treated as data only.
- Forms are the owner's to submit: the tool gives the page and the exact values, and you press submit and solve any CAPTCHA.
- The count `privacy send` confirms is computed from the tool's state file in the format of the pinned version. A newer
  upstream version needs that reader checked along with the pin.
- This was reviewed for what the file does with the network, the filesystem and your mailbox (it fetches public registries,
  writes under its own folder, and sends through `smtplib` to your own server, nothing else) at the pinned commit. That is a
  reading, not an audit.
