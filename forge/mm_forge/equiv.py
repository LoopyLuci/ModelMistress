"""dusty_equiv: do two GGUF models give the same greedy tokens?

The check that Dusty answers exactly like the model it was made from. Both models are served by a ModelMistress hub
(the bundled llama.cpp); each prompt is completed greedily for `max_tokens` tokens by both, and the token ids and
the logprob of every token are compared.

Arguments:
  reference, candidate   model ids as the hub lists them (model.list)
  hub_home               the home folder of a running hub (its control.json is read), or
  exe, model_dirs        a model-mistress executable to start for the test (with a temporary home that scans
                         `model_dirs`) and stop afterwards
  prompts                a list of strings (default: ten built-in prompts)
  max_tokens (64), ctx_size (2048), gpu_layers (omit: the hub's default), tolerance (1e-3, on logprobs),
  min_tokens (the least tokens each prompt must produce to count unless both models stop on their own, default
  max_tokens)
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path

from .protocol import Emitter

PROMPTS = [
    "The capital of France is",
    "def fibonacci(n):\n    \"\"\"Return the n-th Fibonacci number.\"\"\"\n",
    "Explain in two sentences why the sky is blue.",
    "Translate to German: The weather is nice today, let us go for a walk.",
    "1, 1, 2, 3, 5, 8, 13, 21,",
    "Once upon a time, in a village by the sea, there lived",
    "Q: What is the derivative of x^3 + 2x?\nA:",
    "The three laws of thermodynamics are",
    "SELECT name, COUNT(*) FROM orders GROUP BY",
    "Write a haiku about autumn rain:\n",
]


class Hub:
    def __init__(self, home: Path):
        c = json.loads((home / "control.json").read_text())
        self.url, self.token = c["url"], c["token"]

    def post(self, path: str, body: dict, timeout: float = 3600) -> dict:
        req = urllib.request.Request(self.url + path, json.dumps(body).encode(),
                                     {"Authorization": "Bearer " + self.token, "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"{path}: {e.code} {e.read().decode(errors='replace')[:400]}") from None

    def call(self, op: str, args: dict, timeout: float = 3600) -> dict:
        return self.post(f"/v1/call/{op}", args, timeout)["result"]


def start_hub(exe: str, model_dirs: list[str], em: Emitter) -> tuple[Hub, Path, subprocess.Popen]:
    home = Path(tempfile.mkdtemp(prefix="mm-equiv-", dir=os.environ.get("MM_FORGE_DATA") or None))
    dirs = ", ".join(json.dumps(str(Path(d).as_posix())) for d in model_dirs)
    (home / "config.toml").write_text(f'model_dirs = [{dirs}]\nollama_url = ""\nmax_loaded = 1\n')
    proc = subprocess.Popen([exe, "--home", str(home), "serve"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    for _ in range(300):
        if (home / "control.json").is_file():
            time.sleep(0.5)
            em.log(f"started {exe} with a temporary home")
            return Hub(home), home, proc
        if proc.poll() is not None:
            raise RuntimeError("the hub exited at start")
        time.sleep(0.1)
    proc.kill()
    raise RuntimeError("the hub did not start")


def stop_hub(exe: str, home: Path, proc: subprocess.Popen) -> None:
    try:
        subprocess.run([exe, "--home", str(home), "stop"], timeout=30, capture_output=True)
        proc.wait(timeout=30)
    except Exception:  # noqa: BLE001
        proc.kill()
    shutil.rmtree(home, ignore_errors=True)


def complete(hub: Hub, model: str, prompt: str, max_tokens: int) -> dict:
    r = hub.post("/v1/completions", {"model": model, "prompt": prompt, "max_tokens": max_tokens, "temperature": 0,
                                     "top_k": 1, "seed": 0, "logprobs": 5})
    ch = r["choices"][0]
    content = (ch.get("logprobs") or {}).get("content") or []
    return {"finish": ch.get("finish_reason"), "text": ch["text"], "ids": [t["id"] for t in content], "logprobs": [t["logprob"] for t in content],
            "tops": [[(a["id"], a["logprob"]) for a in t.get("top_logprobs", [])] for t in content]}


def compare(a: dict, b: dict, tolerance: float) -> dict:
    """Token-for-token comparison of two completions."""
    n = min(len(a["ids"]), len(b["ids"]))
    first_diff = next((i for i in range(n) if a["ids"][i] != b["ids"][i]), None)
    if first_diff is None and len(a["ids"]) != len(b["ids"]):
        first_diff = n
    diffs = [abs(x - y) for x, y in zip(a["logprobs"][:n], b["logprobs"][:n])]
    top_diffs = []
    for ta, tb in zip(a["tops"][:n], b["tops"][:n]):
        da, db = dict(ta), dict(tb)
        top_diffs += [abs(da[k] - db[k]) for k in da.keys() & db.keys()]
    mx = max(diffs, default=0.0)
    return {"tokens": [len(a["ids"]), len(b["ids"])], "ids_equal": first_diff is None, "first_diff": first_diff,
            "text_equal": a["text"] == b["text"], "max_logprob_diff": mx,
            "max_top_logprob_diff": max(top_diffs, default=0.0),
            "mean_logprob_diff": sum(diffs) / len(diffs) if diffs else 0.0,
            "within_tolerance": mx <= tolerance}


def run(args: dict, em: Emitter) -> dict:
    t0 = time.time()
    ref, cand = args["reference"], args["candidate"]
    prompts = args.get("prompts") or PROMPTS
    max_tokens = int(args.get("max_tokens", 64))
    min_tokens = int(args.get("min_tokens", max_tokens))
    tol = float(args.get("tolerance", 1e-3))
    started = None
    exe = args.get("exe")
    try:
        if exe:
            hub, home, proc = start_hub(exe, args.get("model_dirs", []), em)
            started = (exe, home, proc)
        else:
            hub = Hub(Path(args["hub_home"]))
        load = {"ctx_size": int(args.get("ctx_size", 2048))}
        if "gpu_layers" in args:
            load["gpu_layers"] = int(args["gpu_layers"])
        outs: dict[str, list[dict]] = {}
        total = 2 * len(prompts)
        done = 0
        for role, model in (("reference", ref), ("candidate", cand)):
            em.log(f"loading {role} {model}")
            loaded = hub.call("model.load", {"model": model, **load})
            em.log(f"{role} loaded in {loaded.get('load_ms')} ms")
            outs[role] = []
            for i, p in enumerate(prompts):
                t = time.time()
                outs[role].append(complete(hub, model, p, max_tokens))
                done += 1
                em.progress(done, total, f"{role}: prompt {i + 1}/{len(prompts)} ({time.time() - t:.0f}s)")
            hub.call("model.unload", {"model": model})
        per = []
        for i, (a, b) in enumerate(zip(outs["reference"], outs["candidate"])):
            c = compare(a, b, tol)
            c["prompt"] = prompts[i][:60]
            # a prompt that both models answer by stopping early (end of text) still counts, with fewer tokens
            c["enough_tokens"] = min(c["tokens"]) >= min_tokens or (a["finish"] == "stop" and b["finish"] == "stop")
            per.append(c)
            em.metric("ids_equal", 1.0 if c["ids_equal"] else 0.0, step=i)
            em.metric("max_logprob_diff", c["max_logprob_diff"], step=i)
            em.metric("tokens", min(c["tokens"]), step=i)
        ok = all(c["ids_equal"] and c["within_tolerance"] and c["enough_tokens"] for c in per)
        return {
            "ok": ok, "reference": ref, "candidate": cand, "prompts": len(prompts), "max_tokens": max_tokens,
            "tolerance": tol,
            "identical_token_ids": sum(c["ids_equal"] for c in per),
            "identical_text": sum(c["text_equal"] for c in per),
            "total_tokens_compared": sum(min(c["tokens"]) for c in per),
            "max_logprob_diff": max(c["max_logprob_diff"] for c in per),
            "max_top_logprob_diff": max(c["max_top_logprob_diff"] for c in per),
            "per_prompt": per, "elapsed_s": round(time.time() - t0, 1),
        }
    finally:
        if started:
            stop_hub(*started)
