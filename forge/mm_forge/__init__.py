"""ModelMistress Forge: the managed Python worker of ModelMistress's training system.

The hub starts `python -m mm_forge <step>` for every pipeline step, hands it a JSON object on stdin and reads JSON
lines from stdout (see `protocol`). The worker computes; the hub owns scheduling, data, versions and telemetry.
"""

__version__ = "0.1.0"
