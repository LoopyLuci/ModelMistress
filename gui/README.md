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
- **Chat**: streamed replies from a chat model, an optional system prompt. Ctrl+Enter sends, Stop (or Esc) stops. A
  model's markup stays visible as text instead of being rendered.
- **Settings**: two parts. **Appearance** sets the theme (follow the system, light or dark), the style (Fusion, the
  default, which follows the theme on every platform, or another Qt style) and the text size. It is kept on this computer, applied at once, and never sent to the
  hub. **Hub settings** are the hub's own; a filter narrows the list, and Save sends only the values that changed,
  checked against their type. A secret is not shown.

Ctrl+1 to Ctrl+4 switch pages. The Status page refreshes when it is opened. The window remembers its size and place.

Not in this window yet: pulling models, loading with options (context size, GPU layers), peer keys and usage, and the
dashboard's other pages (traces, knowledge modules, factory, speech, images, vision). The dashboard at `/ui` has the
pages it has; it has no page for peer keys or usage, which are on the command line (`keys`, `usage`) and in the API.

## Test

```bash
python -m pip install -r requirements-dev.txt
python -m pytest tests -q
```

Run from this folder. The tests use an offscreen Qt platform and a local stand-in for the hub. The client test
against a real hub runs when `MM_HUB_EXE` names a `model-mistress` binary (it starts that binary in a throwaway
home). `python ci/pipeline.py --only gui` runs this suite and fails when the installed PySide6 or pytest differ from
the pins in `requirements*.txt`.
