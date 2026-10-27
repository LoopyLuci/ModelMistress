# ModelMistress window (Python)

A desktop window written entirely in Python with PySide6 (Qt 6). It is a client of the hub: it reads the hub's
control file in the home folder and calls the hub's HTTP API, the same operations the dashboard and MCP use. The hub
stays a Rust program with no Python dependency, so the window only needs the hub to be running.

## Run

Start the hub, then, from this folder:

```bash
python -m pip install -r requirements.txt
python -m mm_gui
```

`python -m mm_gui --home <folder>` names the hub's home folder; without it the window uses `MM_HOME`, else the
platform's standard folder, the same rule the hub uses.

## Pages

- **Status**: version, process, uptime, engine, models known, loaded models, memory.
- **Models**: the models on this machine, a filter, load and unload for the selected model, and a disk rescan.
- **Chat**: streamed replies from a chat model, an optional system prompt, Stop (or Esc). A model's markup stays
  visible as text instead of being rendered.
- **Settings**: the hub's settings. Save sends only the values that changed, checked against their type. A secret is
  not shown.

Pulling models and the dashboard's other pages (traces, knowledge modules, factory, speech, images, vision, keys and
usage) are not in this window yet; the dashboard at `/ui` has them.

## Test

```bash
python -m pip install -r requirements-dev.txt
python -m pytest tests -q
```

Run from this folder. The tests use an offscreen Qt platform and a local stand-in for the hub. The client test
against a real hub runs when `MM_HUB_EXE` names a `model-mistress` binary (it starts that binary in a throwaway
home). `python ci/pipeline.py --only gui` runs this suite and fails when the installed PySide6 or pytest differ from
the pins in `requirements*.txt`.
