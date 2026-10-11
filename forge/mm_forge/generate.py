"""Serving a Hugging Face model (safetensors / PyTorch) for generation, on the GPU (ROCm) or the CPU.

This is the way the factory runs a model whose architecture llama.cpp cannot: PyTorch and `transformers` run it.
Two steps use the code here:

* `generate`: one shot. Arguments: `model_path`, `device` (auto|gpu|cpu), `dtype`, and `requests` (a list of request
  objects) or one request's own fields. The result holds `info` and `results`.
* `generate_serve`: stays up. The first stdin line is the same arguments (without requests); the worker answers it
  with a result `{"id": "init", ...info}`. After that, each stdin line is one request and each is answered by one
  result line with the same `id`:

      {"id": "7", "op": "generate", "prompt": "Hello", "max_tokens": 32, "temperature": 0, "top_logprobs": 3}
      {"type":"result","data":{"id":"7","text":"...","tokens":[{"id":12,"text":" world","logprob":-0.3,
                              "top":[{"id":12,"text":" world","logprob":-0.3}, ...]}], "finish":"length",
                              "prompt_tokens":1,"device":"cuda:0"}}

  `{"id": "x", "op": "shutdown"}` (or closing stdin) ends it. A request that fails is answered with
  `{"id": ..., "error": "..."}`, and the server stays up.

A request gives `prompt` (text), `prompt_ids` (token ids, needs no tokenizer) or `messages` (chat; needs the
tokenizer's chat template), `max_tokens` (default 64), `temperature` (0 = greedy), `top_p`, `seed`, `top_logprobs`
(0..20) and `stop_ids`. Logprobs are of the model's own distribution (before temperature and top-p).
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from .protocol import Emitter, read_args

INTERACTIVE = False  # (genserve sets True: the first stdin line holds its arguments, see __main__)


def pick_device(requested: str, need_bytes: int, has_gpu: bool, free_bytes: int = 0) -> str:
    """`gpu` and `cpu` are obeyed (`gpu` without a GPU is an error); `auto` takes the GPU when the weights, plus a
    fifth for the cache and activations, fit in its free memory."""
    requested = (requested or "auto").lower()
    if requested == "cpu":
        return "cpu"
    if requested in ("gpu", "cuda"):
        if not has_gpu:
            raise RuntimeError("device=gpu was asked for, but no GPU is usable")
        return "cuda:0"
    if requested != "auto":
        raise ValueError("device must be auto, gpu or cpu")
    return "cuda:0" if has_gpu and need_bytes * 1.2 <= free_bytes else "cpu"


def weights_bytes(path: Path) -> int:
    files = list(path.glob("*.safetensors")) or list(path.glob("pytorch_model*.bin"))
    return sum(f.stat().st_size for f in files)


class Model:
    def __init__(self, init: dict):
        import torch
        from transformers import AutoModelForCausalLM

        self.torch = torch
        path = Path(init["model_path"])
        has_gpu = bool(torch.cuda.is_available())
        free = torch.cuda.mem_get_info(0)[0] if has_gpu else 0
        need = weights_bytes(path)
        self.device = pick_device(init.get("device", "auto"), need, has_gpu, free)
        dtype = init.get("dtype", "auto")
        if dtype == "auto":
            dtype = "bfloat16" if self.device != "cpu" else "float32"
        self.dtype = getattr(torch, dtype)
        self.model = AutoModelForCausalLM.from_pretrained(str(path), torch_dtype=self.dtype, low_cpu_mem_usage=True)
        self.model.to(self.device).eval()
        self.tok = None
        if any((path / f).is_file() for f in ("tokenizer.json", "tokenizer.model", "vocab.json", "tokenizer_config.json")):
            try:
                from transformers import AutoTokenizer

                self.tok = AutoTokenizer.from_pretrained(str(path))
            except Exception:  # noqa: BLE001  (a model without a usable tokenizer still serves token ids)
                pass
        eos = self.model.generation_config.eos_token_id if self.model.generation_config else None
        self.eos = set([eos] if isinstance(eos, int) else (eos or []))
        self.info = {
            "model_path": str(path),
            "architecture": (self.model.config.architectures or [type(self.model).__name__])[0],
            "device": self.device,
            "device_requested": init.get("device", "auto"),
            "dtype": dtype,
            "weights_bytes": need,
            "parameters": sum(p.numel() for p in self.model.parameters()),
            "tokenizer": self.tok is not None,
            "gpu_available": has_gpu,
            "torch": torch.__version__,
        }

    def prompt_ids(self, req: dict) -> list[int]:
        if req.get("prompt_ids") is not None:
            return [int(i) for i in req["prompt_ids"]]
        if self.tok is None:
            raise ValueError("this model has no tokenizer: give prompt_ids")
        if req.get("messages") is not None:
            text = self.tok.apply_chat_template(req["messages"], add_generation_prompt=True, tokenize=False)
            return self.tok(text, add_special_tokens=False)["input_ids"]
        if req.get("prompt") is None:
            raise ValueError("give prompt, prompt_ids or messages")
        return self.tok(str(req["prompt"]))["input_ids"]

    def text(self, ids: list[int]) -> str:
        return self.tok.decode(ids) if self.tok is not None else ""

    def generate(self, req: dict) -> dict:
        torch = self.torch
        ids = self.prompt_ids(req)
        if not ids:
            raise ValueError("the prompt is empty")
        max_tokens = int(req.get("max_tokens", 64))
        temperature = float(req.get("temperature", 0.0))
        top_p = float(req.get("top_p", 1.0))
        n_top = max(0, min(int(req.get("top_logprobs", 0)), 20))
        stop = set(int(i) for i in req.get("stop_ids", [])) | self.eos
        gen = None
        if temperature > 0:
            gen = torch.Generator(device="cpu").manual_seed(int(req.get("seed", 0)))
        tokens, finish = [], "length"
        with torch.inference_mode():
            x = torch.tensor([ids], device=self.device)
            out = self.model(input_ids=x, use_cache=True)
            for _ in range(max_tokens):
                logits = out.logits[0, -1].float()
                logp = torch.log_softmax(logits, dim=-1)
                if temperature <= 0:
                    nxt = int(torch.argmax(logits))
                else:
                    probs = torch.softmax(logits / temperature, dim=-1).cpu()
                    if top_p < 1.0:
                        sp, si = torch.sort(probs, descending=True)
                        keep = torch.cumsum(sp, 0) - sp < top_p
                        sp = sp * keep
                        probs = torch.zeros_like(probs).scatter_(0, si, sp / sp.sum())
                    nxt = int(torch.multinomial(probs, 1, generator=gen))
                entry = {"id": nxt, "text": self.text([nxt]), "logprob": float(logp[nxt])}
                if n_top:
                    tv, ti = torch.topk(logp, n_top)
                    entry["top"] = [{"id": int(i), "text": self.text([int(i)]), "logprob": float(v)}
                                    for v, i in zip(tv.tolist(), ti.tolist())]
                tokens.append(entry)
                if nxt in stop:
                    finish = "stop"
                    break
                out = self.model(input_ids=torch.tensor([[nxt]], device=self.device),
                                 past_key_values=out.past_key_values, use_cache=True)
        return {"text": self.text([t["id"] for t in tokens]), "tokens": tokens, "finish": finish,
                "prompt_tokens": len(ids), "completion_tokens": len(tokens), "device": self.device}


def run(args: dict, em: Emitter) -> dict:
    """The one-shot step."""
    m = Model(args)
    reqs = args.get("requests")
    if reqs is None:
        reqs = [{k: v for k, v in args.items() if k not in ("model_path", "device", "dtype")}]
    results = []
    for i, r in enumerate(reqs):
        results.append(m.generate(r))
        em.progress(i + 1, len(reqs), "generating")
    return {"info": m.info, "results": results}


def serve(init: dict, em: Emitter, stdin=None) -> dict:
    """The long-running step: see the module docstring."""
    stdin = stdin or sys.stdin
    m = Model(init)
    em.result({"id": "init", **m.info})
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        rid = None
        try:
            req = json.loads(line)
            rid = req.get("id")
            if req.get("op") == "shutdown":
                break
            em.result({"id": rid, **m.generate(req)})
        except Exception as e:  # noqa: BLE001
            em.result({"id": rid, "error": f"{type(e).__name__}: {e}"})
    return {"stopped": True}
