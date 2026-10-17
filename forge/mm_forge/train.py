"""LoRA / QLoRA fine-tuning (steps `lora_train` and `qlora_train`), on PyTorch + PEFT, GPU (ROCm or CUDA) or CPU.

A plain training loop on purpose (AdamW, warmup + cosine, gradient accumulation, bf16 autocast, gradient
checkpointing, length-grouped batches, loss on the assistant's turns only), so it does not depend on a trainer
library's moving API. The approach is the one of ABP's PEFT worker, folded into the Forge's protocol, checkpoints and
run store.

Arguments (JSON): `base_model` (folder or repo id), `data` (file or list of files, see `traindata`), `out_dir`;
LoRA: `rank` 16, `alpha` 16, `dropout` 0, `target_modules` (default: the attention and MLP projections present),
`rslora`, `dora`; optimisation: `learning_rate` 2e-4, `epochs` 1 or `max_steps`, `batch_size` 2, `grad_accum` 4,
`max_seq_length` 1024, `warmup_steps`, `weight_decay`, `max_grad_norm` 1; `load_in_4bit` (QLoRA; always on for
`qlora_train`), `dtype` (auto|bfloat16|float16|float32), `device` (auto|gpu|cpu), `gradient_checkpointing` true,
`eval_fraction` 0, `eval_every`, `save_every` 50, `keep_checkpoints` 2, `resume` true, `seed` 3407, `merge` false
(also write the merged model to `<out_dir>/merged`), `free_vram_gb` (override the measured value), `vram_check` true.

Output: `<out_dir>/adapter` (PEFT adapter + tokenizer), `<out_dir>/checkpoints/ckpt-<step>` (adapter tensors in the
Forge's safetensors + hash-manifest format, plus the optimizer state and the position in the data), `train.json`.
Starting again with the same `out_dir` resumes from the newest checkpoint whose hash verifies. Metrics: `loss`, `lr`,
`grad_norm`, `eval_loss`, `gpu_mem_gb`, each with its step.

QLoRA needs `bitsandbytes` with a working backend for the GPU. That is Linux + CUDA (or a ROCm build of
bitsandbytes on Linux); it is usually not available on Windows/ROCm. Without it `qlora_train` falls back to 16-bit
LoRA with a warning (and says so in its result), or fails when `qlora_fallback` is false.
"""

from __future__ import annotations

import json
import math
import os
import random
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path

from . import traindata
from .checkpoint import file_sha256, load_checkpoint, save_checkpoint, verify_checkpoint
from .protocol import Emitter

DEFAULT_TARGETS = ["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"]
INSTALL_HINT = "run `forge install` (it installs torch, transformers and peft into the Forge environment)"
Q4_BYTES_PER_PARAM = 0.6  # nf4 weights + quantisation constants + the layers kept in 16 bit


class TrainError(RuntimeError):
    """A failure with a message fit for the user (shown as the run's error)."""


@dataclass
class TrainConfig:
    base_model: str
    data: list[str]
    out_dir: str
    rank: int = 16
    alpha: int = 16
    dropout: float = 0.0
    target_modules: list[str] = field(default_factory=lambda: list(DEFAULT_TARGETS))
    rslora: bool = False
    dora: bool = False
    learning_rate: float = 2e-4
    epochs: float = 1.0
    max_steps: int = 0
    batch_size: int = 2
    grad_accum: int = 4
    max_seq_length: int = 1024
    warmup_steps: int = 0
    weight_decay: float = 0.0
    max_grad_norm: float = 1.0
    load_in_4bit: bool = False
    dtype: str = "auto"
    device: str = "auto"
    gradient_checkpointing: bool = True
    eval_fraction: float = 0.0
    eval_every: int = 0
    save_every: int = 50
    keep_checkpoints: int = 2
    resume: bool = True
    seed: int = 3407
    merge: bool = False
    free_vram_gb: float | None = None
    vram_check: bool = True
    qlora_fallback: bool = True

    @classmethod
    def from_args(cls, args: dict, qlora: bool = False) -> "TrainConfig":
        a = dict(args)
        base = a.pop("base_model", None) or a.pop("model_path", None)
        data = a.pop("data", None) or a.pop("dataset", None)
        out = a.pop("out_dir", None)
        if not base:
            raise TrainError("give base_model (a folder with a Hugging Face model, or a repo id)")
        if not data:
            raise TrainError("give data (a .json / .jsonl file, or a list of them)")
        if not out:
            raise TrainError("give out_dir (where the adapter and the checkpoints go)")
        known = {f for f in cls.__dataclass_fields__}  # noqa: C419
        extra = sorted(set(a) - known)
        if extra:
            raise TrainError(f"unknown argument(s): {', '.join(extra)}")
        cfg = cls(base_model=str(base), data=[data] if isinstance(data, str) else [str(d) for d in data],
                  out_dir=str(out), **a)
        if qlora:
            cfg.load_in_4bit = True
        cfg.validate()
        return cfg

    def validate(self) -> None:
        if self.rank < 1 or self.alpha < 1:
            raise TrainError("rank and alpha must be at least 1")
        if not 0 <= self.dropout < 1:
            raise TrainError("dropout must be in [0, 1)")
        if self.batch_size < 1 or self.grad_accum < 1 or self.max_seq_length < 8:
            raise TrainError("batch_size and grad_accum must be at least 1 and max_seq_length at least 8")
        if self.max_steps < 0 or self.epochs <= 0 and not self.max_steps:
            raise TrainError("give epochs > 0 or max_steps > 0")
        if self.learning_rate <= 0:
            raise TrainError("learning_rate must be positive")
        if self.device not in ("auto", "gpu", "cpu"):
            raise TrainError("device must be auto, gpu or cpu")
        if self.dtype not in ("auto", "bfloat16", "float16", "float32"):
            raise TrainError("dtype must be auto, bfloat16, float16 or float32")


# ---- schedule ----------------------------------------------------------------------------------------------------


def total_steps(cfg: TrainConfig, n_examples: int) -> int:
    if cfg.max_steps:
        return cfg.max_steps
    per_epoch = math.ceil(n_examples / cfg.batch_size / cfg.grad_accum)
    return max(1, math.ceil(per_epoch * cfg.epochs))


def lr_at(step: int, total: int, base: float, warmup: int) -> float:
    """Learning rate for optimizer step `step` (0-based): linear warmup, then cosine to zero."""
    if warmup and step < warmup:
        return base * (step + 1) / warmup
    return base * 0.5 * (1 + math.cos(math.pi * (step - warmup) / max(1, total - warmup)))


# ---- VRAM --------------------------------------------------------------------------------------------------------


def model_dims(base: Path) -> dict:
    """Sizes read from config.json and the weight files (no torch needed)."""
    cfg = {}
    try:
        cfg = json.loads((base / "config.json").read_text())
    except (OSError, ValueError):
        pass
    cfg = cfg.get("text_config", cfg)
    files = list(base.glob("*.safetensors")) or list(base.glob("pytorch_model*.bin"))
    return {
        "weights_bytes": sum(f.stat().st_size for f in files),
        "hidden": int(cfg.get("hidden_size", 2048)),
        "layers": int(cfg.get("num_hidden_layers", 24)),
        "intermediate": int(cfg.get("intermediate_size", 4 * int(cfg.get("hidden_size", 2048)))),
        "vocab": int(cfg.get("vocab_size", 32000)),
    }


def estimate_bytes(dims: dict, batch: int, seq: int, rank: int, q4: bool, checkpointing: bool = True) -> int:
    """A deliberately pessimistic estimate of the peak memory of a LoRA step.

    weights (16 bit, or ~0.6 byte per parameter in 4 bit) + the adapters with their gradients and Adam state
    (about 16 bytes per adapter parameter; a rank-r adapter on the 7 projections of a layer has about
    r * 12 * hidden parameters) + activations (with checkpointing: one saved input per layer and one layer's
    working set) + the logits (bf16, upcast to fp32, and their gradient) + a fixed reserve."""
    h, layers, inter, vocab = dims["hidden"], dims["layers"], dims["intermediate"], dims["vocab"]
    w = dims["weights_bytes"]
    weights = w * (Q4_BYTES_PER_PARAM / 2) if q4 else w
    adapters = 16 * rank * 12 * h * layers
    tokens = batch * seq
    per_layer_saved = tokens * h * 2
    working = tokens * (16 * h + 3 * inter) * 2
    acts = layers * per_layer_saved + working if checkpointing else layers * (per_layer_saved * 12 + working)
    logits = tokens * vocab * (2 + 4 + 4)
    return int(weights + adapters + acts + logits + (512 << 20))


def plan_memory(dims: dict, cfg: TrainConfig, free_bytes: int | None, q4_available: bool) -> dict:
    """Picks (4-bit?, batch size, grad_accum) that fits `free_bytes`, keeping batch_size * grad_accum.

    Order of preference: what was asked for; then a smaller batch (more accumulation); then, if QLoRA is possible
    and 16-bit was not insisted on, 4-bit with the biggest batch that fits. `free_bytes=None` (CPU, or unknown)
    accepts the request as it is. Raises TrainError with the numbers when nothing fits."""
    effective = cfg.batch_size * cfg.grad_accum
    if free_bytes is None:
        return {"load_in_4bit": cfg.load_in_4bit, "batch_size": cfg.batch_size, "grad_accum": cfg.grad_accum,
                "estimate_bytes": None, "free_bytes": None, "changed": False}
    budget = int(free_bytes * 0.92)
    modes = [cfg.load_in_4bit] if cfg.load_in_4bit else [False] + ([True] if q4_available else [])
    tried = []
    for q4 in modes:
        b = cfg.batch_size
        while b >= 1:
            need = estimate_bytes(dims, b, cfg.max_seq_length, cfg.rank, q4, cfg.gradient_checkpointing)
            tried.append((q4, b, need))
            if need <= budget:
                accum = max(1, math.ceil(effective / b))
                changed = (q4 != cfg.load_in_4bit) or b != cfg.batch_size
                return {"load_in_4bit": q4, "batch_size": b, "grad_accum": accum, "estimate_bytes": need,
                        "free_bytes": free_bytes, "changed": changed}
            b //= 2
    least = min(t[2] for t in tried)
    hint = "" if q4_available or cfg.load_in_4bit else " 4-bit (QLoRA) is not available here."
    raise TrainError(
        f"not enough free VRAM: {free_bytes / 2**30:.1f} GiB free, at least {least / 2**30:.1f} GiB needed for "
        f"{Path(cfg.base_model).name} at max_seq_length {cfg.max_seq_length} with batch 1.{hint} Free GPU memory, "
        "shorten max_seq_length, use a smaller model" + ("" if cfg.load_in_4bit else ", or use a 4-bit run") + ".")


def free_vram_bytes(torch) -> int | None:
    if not torch.cuda.is_available():
        return None
    return int(torch.cuda.mem_get_info(0)[0])


def bitsandbytes_status(torch=None) -> tuple[bool, str]:
    """(usable, why not). bitsandbytes must import and a GPU must be present; on Windows with ROCm it generally
    cannot be installed."""
    try:
        import bitsandbytes  # noqa: F401, PLC0415
    except Exception as e:  # noqa: BLE001  (ImportError, or a native-library failure)
        return False, f"bitsandbytes is not usable here ({type(e).__name__}); QLoRA needs it (Linux + CUDA/ROCm)"
    if torch is not None and not torch.cuda.is_available():
        return False, "bitsandbytes needs a GPU"
    return True, ""


# ---- checkpoints -------------------------------------------------------------------------------------------------


def checkpoint_dir(out_dir: str | Path) -> Path:
    return Path(out_dir) / "checkpoints"


def list_checkpoints(out_dir: str | Path) -> list[tuple[int, Path]]:
    d = checkpoint_dir(out_dir)
    found = []
    if d.is_dir():
        for p in d.glob("ckpt-*"):
            try:
                found.append((int(p.name.split("-", 1)[1]), p))
            except ValueError:
                pass
    return sorted(found)


def find_resume(out_dir: str | Path) -> tuple[int, Path] | None:
    """The newest checkpoint that verifies (hash of the weights and of the optimizer state)."""
    for step, p in reversed(list_checkpoints(out_dir)):
        if verify_checkpoint(p):
            try:
                meta = json.loads((p / "manifest.json").read_text())["meta"]
                opt = p / "optim.pt"
                if not opt.is_file() or file_sha256(opt) == meta.get("optim_sha256"):
                    return step, p
            except (OSError, ValueError, KeyError):
                pass
    return None


def save_train_checkpoint(out_dir: str | Path, step: int, tensors: dict, optimizer_state, state: dict,
                          keep: int = 2) -> Path:
    import torch

    p = checkpoint_dir(out_dir) / f"ckpt-{step}"
    p.mkdir(parents=True, exist_ok=True)
    opt = p / "optim.pt"
    torch.save(optimizer_state, str(opt) + ".tmp")
    os.replace(str(opt) + ".tmp", opt)
    save_checkpoint(p, tensors, meta={**state, "step": step, "optim_sha256": file_sha256(opt)})
    for s, old in list_checkpoints(out_dir)[:-max(1, keep)]:
        for f in old.glob("*"):
            f.unlink()
        old.rmdir()
    return p


def load_train_checkpoint(path: Path):
    import torch

    tensors, meta = load_checkpoint(path)
    opt = path / "optim.pt"
    state = torch.load(opt, map_location="cpu", weights_only=False) if opt.is_file() else None
    return tensors, state, meta


# ---- the run -----------------------------------------------------------------------------------------------------


def _import_stack():
    try:
        import peft  # noqa: PLC0415
        import torch  # noqa: PLC0415
        import transformers  # noqa: PLC0415
    except ImportError as e:
        raise TrainError(f"{e.name or 'a training package'} is not installed: {INSTALL_HINT}") from e
    return torch, transformers, peft


def pick_dtype(torch, name: str, on_gpu: bool):
    if name != "auto":
        return getattr(torch, name)
    if not on_gpu:
        return torch.float32
    return torch.bfloat16 if torch.cuda.is_bf16_supported() else torch.float16


def run_qlora(args: dict, em: Emitter) -> dict:
    return run(args, em, qlora=True)


def run(args: dict, em: Emitter, qlora: bool = False) -> dict:
    t0 = time.time()
    cfg = TrainConfig.from_args(args, qlora=qlora)
    torch, transformers, peft = _import_stack()
    from transformers import AutoModelForCausalLM, AutoTokenizer  # noqa: PLC0415

    out = Path(cfg.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    has_gpu = bool(torch.cuda.is_available())
    if cfg.device == "gpu" and not has_gpu:
        raise TrainError("device=gpu was asked for, but no GPU is usable by PyTorch (run the preflight)")
    if cfg.device == "auto" and not has_gpu:
        raise TrainError("no GPU is usable by PyTorch; training on the CPU needs device=cpu (it is slow)")
    on_gpu = has_gpu and cfg.device != "cpu"
    dev = "cuda" if on_gpu else "cpu"
    notes: list[str] = []

    # 4-bit: only where bitsandbytes works.
    q4_ok, why = bitsandbytes_status(torch if on_gpu else None)
    if cfg.load_in_4bit and not q4_ok:
        if not cfg.qlora_fallback:
            raise TrainError(why)
        cfg.load_in_4bit = False
        notes.append(f"{why}; training 16-bit LoRA instead")
        em.log(notes[-1], level="warn")

    base = Path(cfg.base_model)
    dims = model_dims(base) if base.is_dir() else None
    plan = {"changed": False, "estimate_bytes": None, "free_bytes": None}
    if cfg.vram_check and on_gpu and dims is not None:
        free = int(cfg.free_vram_gb * 2**30) if cfg.free_vram_gb is not None else free_vram_bytes(torch)
        plan = plan_memory(dims, cfg, free, q4_ok)
        if plan["changed"]:
            em.log(f"VRAM: {plan['free_bytes'] / 2**30:.1f} GiB free; using 4-bit={plan['load_in_4bit']}, "
                   f"batch {plan['batch_size']} x accumulation {plan['grad_accum']}", level="warn")
            cfg.load_in_4bit, cfg.batch_size, cfg.grad_accum = plan["load_in_4bit"], plan["batch_size"], plan["grad_accum"]
        else:
            em.log(f"VRAM: {plan['free_bytes'] / 2**30:.1f} GiB free, about {plan['estimate_bytes'] / 2**30:.1f} GiB "
                   "needed: fits")

    random.seed(cfg.seed)
    torch.manual_seed(cfg.seed)
    dtype = pick_dtype(torch, cfg.dtype, on_gpu)

    tok = AutoTokenizer.from_pretrained(cfg.base_model)
    if tok.pad_token_id is None:
        tok.pad_token = tok.eos_token
    if not getattr(tok, "chat_template", None):
        tok.chat_template = traindata.CHATML
        notes.append("the base model has no chat template: ChatML is used and saved with the result")
        em.log(notes[-1])
    examples, stats = traindata.load_examples(cfg.data)
    train_ex, eval_ex = traindata.split(examples, cfg.eval_fraction, cfg.seed)
    items = traindata.encode_all(tok, train_ex, cfg.max_seq_length)
    evals = traindata.encode_all(tok, eval_ex, cfg.max_seq_length)
    if not items:
        raise TrainError(f"none of the {stats['rows']} rows is usable (formats: alpaca, ShareGPT, chat JSONL)")
    em.log(f"data: {stats['usable']} usable of {stats['rows']} rows {stats['formats']}; "
           f"{len(items)} train, {len(evals)} eval")

    kwargs: dict = {"torch_dtype": dtype}
    if cfg.load_in_4bit:
        kwargs["quantization_config"] = transformers.BitsAndBytesConfig(
            load_in_4bit=True, bnb_4bit_quant_type="nf4", bnb_4bit_compute_dtype=dtype, bnb_4bit_use_double_quant=True)
        kwargs["device_map"] = {"": 0}
    model = AutoModelForCausalLM.from_pretrained(cfg.base_model, **kwargs)
    if not cfg.load_in_4bit:
        model.to(dev)
    model.config.use_cache = False
    if cfg.load_in_4bit:
        model = peft.prepare_model_for_kbit_training(model, use_gradient_checkpointing=cfg.gradient_checkpointing)
    elif cfg.gradient_checkpointing:
        model.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
        model.enable_input_require_grads()
    present = {n.rsplit(".", 1)[-1] for n, _ in model.named_modules()}
    targets = [t for t in cfg.target_modules if t in present] or "all-linear"
    model = peft.get_peft_model(model, peft.LoraConfig(
        r=cfg.rank, lora_alpha=cfg.alpha, lora_dropout=cfg.dropout, target_modules=targets, use_rslora=cfg.rslora,
        use_dora=cfg.dora, task_type="CAUSAL_LM"))
    params = {n: p for n, p in model.named_parameters() if p.requires_grad}
    for p in params.values():
        if p.dtype != torch.float32:
            p.data = p.data.float()  # adapters train in fp32 (autocast does the 16-bit maths)
    trainable = sum(p.numel() for p in params.values())
    opt = torch.optim.AdamW(list(params.values()), lr=cfg.learning_rate, weight_decay=cfg.weight_decay)

    total = total_steps(cfg, len(items))
    warm = cfg.warmup_steps or max(1, total // 20)
    lengths = [len(x[0]) for x in items]
    micro_per_epoch = math.ceil(len(items) / cfg.batch_size)

    step, epoch, skip = 0, 0, 0  # skip: micro-batches of `epoch` already done
    resumed = None
    if cfg.resume:
        found = find_resume(out)
        if found:
            tensors, ostate, meta = load_train_checkpoint(found[1])
            missing = set(params) - set(tensors)
            if missing:
                raise TrainError(f"checkpoint {found[1].name} does not match this adapter configuration "
                                 f"({len(missing)} tensors missing); change out_dir or set resume=false")
            with torch.no_grad():
                for n, p in params.items():
                    p.copy_(tensors[n].to(p.device, p.dtype))
            if ostate is not None:
                opt.load_state_dict(ostate)
            step, epoch, skip = int(meta["step"]), int(meta.get("epoch", 0)), int(meta.get("micro_in_epoch", 0))
            resumed = found[0]
            em.log(f"resuming from step {step} ({found[1].name})")

    em.log(f"training: {trainable:,} trainable parameters, {total} steps, batch {cfg.batch_size} x "
           f"{cfg.grad_accum}, lr {cfg.learning_rate}, {dtype}, device {dev}")

    def loss_of(batch):
        ids, lab, att = (t.to(dev) for t in batch)
        with torch.autocast(dev, dtype=dtype, enabled=on_gpu):
            return model(input_ids=ids, attention_mask=att, labels=lab).loss

    def evaluate():
        if not evals:
            return None
        model.eval()
        tot, n = 0.0, 0
        with torch.no_grad():
            for b in traindata.batch_order(len(evals), cfg.batch_size, 0):
                tot += float(loss_of(traindata.collate(evals, b, tok.pad_token_id)))
                n += 1
        model.train()
        return tot / max(1, n)

    def snapshot():
        save_train_checkpoint(out, step, {n: p.detach() for n, p in params.items()}, opt.state_dict(),
                              {"epoch": epoch, "micro_in_epoch": micro_in_epoch, "config": asdict(cfg)},
                              keep=cfg.keep_checkpoints)

    model.train()
    micro_in_epoch = skip
    hist: list[float] = []
    start_step = step
    try:
        while step < total:
            order = traindata.batch_order(len(items), cfg.batch_size, cfg.seed + epoch, lengths)
            for i in range(micro_in_epoch, len(order)):
                loss = loss_of(traindata.collate(items, order[i], tok.pad_token_id))
                (loss / cfg.grad_accum).backward()
                hist.append(loss.item())
                micro_in_epoch = i + 1
                if micro_in_epoch % cfg.grad_accum and micro_in_epoch < len(order):
                    continue
                lr = lr_at(step, total, cfg.learning_rate, warm)
                for g in opt.param_groups:
                    g["lr"] = lr
                gn = float(torch.nn.utils.clip_grad_norm_(list(params.values()), cfg.max_grad_norm))
                opt.step()
                opt.zero_grad(set_to_none=True)
                step += 1
                em.metric("loss", sum(hist) / len(hist), step)
                em.metric("lr", lr, step)
                em.metric("grad_norm", gn, step)
                if on_gpu:
                    em.metric("gpu_mem_gb", torch.cuda.max_memory_allocated() / 2**30, step)
                hist = []
                em.progress(step, total, f"step {step} of {total}")
                if cfg.eval_every and step % cfg.eval_every == 0 and evals:
                    em.metric("eval_loss", evaluate(), step)
                if cfg.save_every and step % cfg.save_every == 0 and step < total:
                    snapshot()
                if step >= total:
                    break
            else:
                epoch += 1
                micro_in_epoch = 0
    except KeyboardInterrupt:
        snapshot()
        raise
    if micro_in_epoch >= micro_per_epoch:
        epoch, micro_in_epoch = epoch + 1, 0
    snapshot()
    final_eval = evaluate()
    if final_eval is not None:
        em.metric("eval_loss", final_eval, step)

    adapter = out / "adapter"
    model.save_pretrained(adapter)
    tok.save_pretrained(adapter)
    em.artifact("adapter", str(adapter), kind="lora-adapter")
    result = {"adapter": str(adapter), "steps": step, "steps_this_run": step - start_step, "resumed_from": resumed,
              "trainable_params": trainable, "examples": len(items), "eval_examples": len(evals), "data": stats,
              "load_in_4bit": cfg.load_in_4bit, "dtype": str(dtype).replace("torch.", ""), "device": dev,
              "vram_plan": plan, "notes": notes, "final_eval_loss": final_eval,
              "config": asdict(cfg), "elapsed_s": round(time.time() - t0, 1)}
    if cfg.merge:
        from .export import merge_adapter  # noqa: PLC0415

        del model, opt
        result["merged"] = merge_adapter(cfg.base_model, str(adapter), str(out / "merged"), em, dtype="auto")
    (out / "train.json").write_text(json.dumps(result, indent=2, default=str))
    return result
