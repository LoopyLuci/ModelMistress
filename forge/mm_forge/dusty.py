"""dusty_convert: Qwen3.5-4B -> Dusty, a `qwen35moe` model that answers exactly like the original.

Every dense FFN becomes the always-on **shared expert** of a mixture-of-experts layer:

* its sigmoid gate (`mlp.shared_expert_gate`) gets all-zero weights, so the gate is sigmoid(0) = 0.5 for every token;
* its down projection is doubled, so 0.5 * (2 * W_down) h = W_down h exactly (a power of two is exact in floating
  point);
* the layer gets `slots` routed experts (default 64, top-4), each 1/8 of the FFN wide, whose down projections are all
  zero (they add exactly nothing), a small random router, and gate/up weights taken from slices of the dense FFN
  (so training the down projections starts from useful features).

llama.cpp's `qwen35moe` computes `moe_out + sigmoid(gate_inp_shexp @ x) * shexp(x)` (src/models/qwen35moe.cpp,
`build_layer_ffn`), the same as the Hugging Face Qwen3-Next/3.5 sparse MoE block, with the top-k weights normalised;
zero experts therefore contribute exactly 0 whatever the router picks.

The text model is kept. The vision tower and the multi-token-prediction (MTP) head are dropped and the manifest
lists them. The output is a Hugging Face style checkpoint (`hf/`), GGUF files converted by llama.cpp's own
`convert_hf_to_gguf.py` (`gguf/`), and `dusty.json` (provenance, what was dropped, sizes, hashes).

With 64 slots the routed experts are about 36 GB in bf16, mostly zeros. `experts="write"` writes them all into the
checkpoint (all-zero tensors as sparse holes); `experts="virtual"` leaves them out of `hf/` (it keeps a
`dusty_virtual.json` that says how to make them) and generates them while converting to GGUF; both give the same
GGUF.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path

from .checkpoint import Spec, file_sha256, write_sharded
from .protocol import Emitter
from .seeding import derive_seed

DUSTY_ARCH = "Qwen3_5MoeForCausalLM"
VIRTUAL_FILE = "dusty_virtual.json"
TOKENIZER_FILES = ("tokenizer.json", "tokenizer_config.json", "vocab.json", "merges.txt", "chat_template.jinja",
                   "generation_config.json", "special_tokens_map.json")


@dataclass
class DustyConfig:
    slots: int = 64            # routed experts per layer
    top_k: int = 4             # routed experts used per token
    ff_divisor: int = 8        # each routed expert is 1/ff_divisor of the dense FFN wide
    seed: int = 20261010       # for the routers
    router_std: float = 0.02   # the routers' standard deviation
    expert_init: str = "slice"  # routed gate/up: "slice" of the dense FFN, or "random"
    expert_std: float = 0.02   # for expert_init="random"

    def validate(self, intermediate: int) -> None:
        if self.slots < self.top_k or self.top_k < 1:
            raise ValueError("slots must be at least top_k, and top_k at least 1")
        if intermediate % self.ff_divisor:
            raise ValueError(f"the FFN width {intermediate} is not divisible by ff_divisor {self.ff_divisor}")
        if self.expert_init not in ("slice", "random"):
            raise ValueError("expert_init must be 'slice' or 'random'")


# ---- names and config ---------------------------------------------------------------------------------------------

_LAYER = re.compile(r"^model\.(?:language_model\.)?layers\.(\d+)\.(.+)$")


def classify(name: str) -> tuple[str, str | None]:
    """How a source tensor is treated: ("keep", new_name), ("ffn", new_name), ("double", new_name), or
    ("drop", reason). `ffn` tensors are copied as they are into the shared expert, `double` is the down projection."""
    if name.startswith("model.visual.") or name.startswith("visual."):
        return "drop", "vision tower"
    if name.startswith("mtp."):
        return "drop", "multi-token prediction head"
    if name == "lm_head.weight":
        return "keep", name
    m = _LAYER.match(name)
    if m:
        layer, rest = m.groups()
        if rest in ("mlp.gate_proj.weight", "mlp.up_proj.weight"):
            return "ffn", f"model.layers.{layer}.mlp.shared_expert.{rest.split('.')[1]}.weight"
        if rest == "mlp.down_proj.weight":
            return "double", f"model.layers.{layer}.mlp.shared_expert.down_proj.weight"
        return "keep", f"model.layers.{layer}.{rest}"
    for src, dst in (("model.language_model.embed_tokens.weight", "model.embed_tokens.weight"),
                     ("model.language_model.norm.weight", "model.norm.weight"),
                     ("model.embed_tokens.weight", "model.embed_tokens.weight"),
                     ("model.norm.weight", "model.norm.weight")):
        if name == src:
            return "keep", dst
    return "drop", "unknown tensor"


def text_config(src: dict) -> dict:
    """The language model's own config: the nested `text_config` of a multimodal checkpoint, else the config itself."""
    if "text_config" not in src:
        return dict(src)
    t = dict(src["text_config"])
    if "tie_word_embeddings" in src:
        t.setdefault("tie_word_embeddings", src["tie_word_embeddings"])
    return t


def build_config(src: dict, cfg: DustyConfig) -> dict:
    """The Hugging Face config of Dusty (Qwen3.5-MoE text model), from the source's config."""
    t = text_config(src)
    inter = int(t["intermediate_size"])
    cfg.validate(inter)
    keep = {k: v for k, v in t.items()
            if k not in ("model_type", "architectures", "mtp_num_hidden_layers", "mtp_use_dedicated_embeddings")}
    out = {
        "architectures": [DUSTY_ARCH],
        "model_type": "qwen3_5_moe_text",
        **keep,
        "num_experts": cfg.slots,
        "num_experts_per_tok": cfg.top_k,
        "moe_intermediate_size": inter // cfg.ff_divisor,
        "shared_expert_intermediate_size": inter,
        "norm_topk_prob": True,
        "decoder_sparse_step": 1,
        "mlp_only_layers": [],
        "router_aux_loss_coef": 0.001,
        "output_router_logits": False,
        "mtp_num_hidden_layers": 0,
        "tie_word_embeddings": bool(t.get("tie_word_embeddings", src.get("tie_word_embeddings", True))),
    }
    return out


# ---- the tensors ---------------------------------------------------------------------------------------------------


def double(torch, w):
    """2*w, exactly: scaling by a power of two only changes the exponent. Refuses a result that overflowed."""
    out = w * 2
    if not bool(torch.isfinite(out).all()):
        raise ValueError("doubling the down projection overflowed")
    return out


def router_weight(torch, cfg: DustyConfig, layer: int, hidden: int, dtype):
    g = torch.Generator().manual_seed(derive_seed(cfg.seed, "router", layer))
    return (torch.randn(cfg.slots, hidden, generator=g) * cfg.router_std).to(dtype)


def routed_gate_up(torch, cfg: DustyConfig, layer: int, gate, up, dtype):
    """[slots, 2*ffn_exp, hidden]: gate rows then up rows per slot. `slice`: slot s takes the (s mod divisor)-th
    1/divisor of the dense FFN's rows; `random`: small random weights."""
    ie = gate.shape[0] // cfg.ff_divisor
    hidden = gate.shape[1]
    if cfg.expert_init == "slice":
        g = gate.reshape(cfg.ff_divisor, ie, hidden)
        u = up.reshape(cfg.ff_divisor, ie, hidden)
        idx = torch.arange(cfg.slots) % cfg.ff_divisor
        return torch.cat([g[idx], u[idx]], dim=1).to(dtype).contiguous()
    gen = torch.Generator().manual_seed(derive_seed(cfg.seed, "experts", layer))
    return (torch.randn(cfg.slots, 2 * ie, hidden, generator=gen) * cfg.expert_std).to(dtype)


class Source:
    """The source checkpoint's tensors, read lazily."""

    def __init__(self, directory: Path):
        from safetensors import safe_open

        self.dir = Path(directory)
        self._open = safe_open
        idx = self.dir / "model.safetensors.index.json"
        if idx.is_file():
            self.weight_map: dict[str, str] = json.loads(idx.read_text())["weight_map"]
        else:
            files = sorted(self.dir.glob("*.safetensors"))
            self.weight_map = {}
            for f in files:
                with safe_open(str(f), framework="pt") as h:
                    for k in h.keys():
                        self.weight_map[k] = f.name
        self._handles: dict[str, object] = {}

    def _h(self, fname: str):
        if fname not in self._handles:
            self._handles[fname] = self._open(str(self.dir / fname), framework="pt")
        return self._handles[fname]

    def names(self) -> list[str]:
        return list(self.weight_map)

    def info(self, name: str) -> tuple[str, tuple[int, ...]]:
        s = self._h(self.weight_map[name]).get_slice(name)
        return s.get_dtype(), tuple(s.get_shape())

    def get(self, name: str):
        return self._h(self.weight_map[name]).get_tensor(name)


_ST = {"BF16": 2, "F16": 2, "F32": 4, "F64": 8, "I64": 8, "I32": 4, "U8": 1, "I8": 1, "BOOL": 1}


def _nbytes(dtype: str, shape) -> int:
    n = _ST[dtype]
    for s in shape:
        n *= s
    return n


def plan(src: Source, src_config: dict, cfg: DustyConfig, with_experts: bool) -> tuple[list[Spec], dict]:
    """The Dusty checkpoint as tensor specs, and what was dropped. Nothing is read yet (the specs make tensors on
    demand)."""
    import torch

    t = text_config(src_config)
    layers = int(t["num_hidden_layers"])
    hidden = int(t["hidden_size"])
    inter = int(t["intermediate_size"])
    cfg.validate(inter)
    ie = inter // cfg.ff_divisor
    specs: list[Spec] = []
    dropped: dict[str, dict] = {}
    dtypes = {"BF16": torch.bfloat16, "F16": torch.float16, "F32": torch.float32}
    act_dtype = "BF16"
    for name in sorted(src.names(), key=_sort_key):
        kind, new = classify(name)
        dt, shape = src.info(name)
        if kind == "drop":
            d = dropped.setdefault(new, {"tensors": 0, "bytes": 0, "example": name})
            d["tensors"] += 1
            d["bytes"] += _nbytes(dt, shape)
            continue
        if kind == "double":
            specs.append(Spec(new, dt, shape, (lambda n=name: double(torch, src.get(n)))))
        else:
            specs.append(Spec(new, dt, shape, (lambda n=name: src.get(n))))
    # per layer: gate (zero), router, routed experts; keep the order by layer
    extra: list[Spec] = []
    for layer in range(layers):
        pre = f"model.layers.{layer}.mlp"
        gate_name = f"{pre}.shared_expert.gate_proj.weight"
        gdt, _ = src.info(_source_name(src, gate_name))
        act_dtype = gdt
        td = dtypes[gdt]
        extra.append(Spec(f"{pre}.shared_expert_gate.weight", gdt, (1, hidden), None))
        extra.append(Spec(f"{pre}.gate.weight", gdt, (cfg.slots, hidden),
                          (lambda l=layer, d=td: router_weight(torch, cfg, l, hidden, d))))
        if with_experts:
            def gate_up(l=layer, d=td):
                g = src.get(_source_name(src, f"model.layers.{l}.mlp.shared_expert.gate_proj.weight"))
                u = src.get(_source_name(src, f"model.layers.{l}.mlp.shared_expert.up_proj.weight"))
                return routed_gate_up(torch, cfg, l, g, u, d)

            extra.append(Spec(f"{pre}.experts.gate_up_proj", gdt, (cfg.slots, 2 * ie, hidden), gate_up))
            extra.append(Spec(f"{pre}.experts.down_proj", gdt, (cfg.slots, hidden, ie), None))
    specs.extend(extra)
    specs.sort(key=lambda s: _sort_key(s.name))
    return specs, {"dropped": dropped, "dtype": act_dtype}


def _source_name(src: Source, dusty_name: str) -> str:
    """The source tensor behind a Dusty shared-expert name."""
    m = re.match(r"^model\.layers\.(\d+)\.mlp\.shared_expert\.(\w+)\.weight$", dusty_name)
    assert m, dusty_name
    layer, proj = m.groups()
    for prefix in ("model.language_model.layers", "model.layers"):
        cand = f"{prefix}.{layer}.mlp.{proj}.weight"
        if cand in src.weight_map:
            return cand
    raise KeyError(dusty_name)


def _sort_key(name: str):
    m = _LAYER.match(name)
    if m:
        return (1, int(m.group(1)), name)
    return (0 if "embed" in name else 2, 0, name)


# ---- the steps -----------------------------------------------------------------------------------------------------


def _free(path: Path) -> int:
    p = path
    while not p.exists():
        p = p.parent
    return shutil.disk_usage(p).free


def estimate_expert_bytes(t: dict, cfg: DustyConfig) -> int:
    inter, hidden, layers = int(t["intermediate_size"]), int(t["hidden_size"]), int(t["num_hidden_layers"])
    ie = inter // cfg.ff_divisor
    return layers * cfg.slots * 3 * ie * hidden * 2


def convert_hf(src_dir: Path, out_dir: Path, cfg: DustyConfig, experts: str, em: Emitter,
               max_shard_bytes: int = 4 << 30) -> dict:
    """Source checkpoint -> Dusty Hugging Face checkpoint in `out_dir`. Returns what was written."""
    src_config = json.loads((src_dir / "config.json").read_text())
    t = text_config(src_config)
    src = Source(src_dir)
    with_experts = experts == "write"
    specs, info = plan(src, src_config, cfg, with_experts)
    out_dir.mkdir(parents=True, exist_ok=True)
    total = len(specs)
    done = 0

    def tick(spec: Spec) -> None:
        nonlocal done
        done += 1
        if done % 16 == 0 or done == total:
            em.progress(done, total, f"hf checkpoint: {done}/{total} tensors")

    em.log(f"hf checkpoint: {total} tensors, experts={experts}, dropped {sorted(info['dropped'])}")
    written = write_sharded(out_dir, specs, max_shard_bytes=max_shard_bytes,
                            metadata={"mm_forge": "dusty"}, on_tensor=tick)
    (out_dir / "config.json").write_text(json.dumps(build_config(src_config, cfg), indent=2))
    for f in TOKENIZER_FILES:
        if (src_dir / f).is_file():
            shutil.copy2(src_dir / f, out_dir / f)
    for f in ("LICENSE",):
        if (src_dir / f).is_file():
            shutil.copy2(src_dir / f, out_dir / f)
    if not with_experts:
        (out_dir / VIRTUAL_FILE).write_text(json.dumps({
            "format": "mm-dusty-virtual/1",
            "note": "the routed experts (mlp.experts.gate_up_proj, mlp.experts.down_proj) are not stored; "
                    "mm_forge.dusty makes them from this and the shared-expert weights",
            "config": asdict(cfg),
            "layers": int(t["num_hidden_layers"]),
            "hidden": int(t["hidden_size"]),
            "intermediate": int(t["intermediate_size"]),
            "dtype": info["dtype"],
        }, indent=2))
    return {**written, "dropped": info["dropped"], "dtype": info["dtype"]}


def virtual_specs(hf_dir: Path) -> list[Spec]:
    """The routed-expert tensors a `virtual` checkpoint leaves out, as specs that make them."""
    import torch

    v = json.loads((hf_dir / VIRTUAL_FILE).read_text())
    cfg = DustyConfig(**v["config"])
    td = {"BF16": torch.bfloat16, "F16": torch.float16, "F32": torch.float32}[v["dtype"]]
    src = Source(hf_dir)
    ie = v["intermediate"] // cfg.ff_divisor
    out = []
    for layer in range(v["layers"]):
        pre = f"model.layers.{layer}.mlp"

        def gate_up(l=layer):
            g = src.get(f"model.layers.{l}.mlp.shared_expert.gate_proj.weight")
            u = src.get(f"model.layers.{l}.mlp.shared_expert.up_proj.weight")
            return routed_gate_up(torch, cfg, l, g, u, td)

        out.append(Spec(f"{pre}.experts.gate_up_proj", v["dtype"], (cfg.slots, 2 * ie, v["hidden"]), gate_up))
        out.append(Spec(f"{pre}.experts.down_proj", v["dtype"], (cfg.slots, v["hidden"], ie), None))
    return out


def find_converter(args: dict) -> Path:
    cands = []
    if args.get("convert_script"):
        cands.append(Path(args["convert_script"]))
    if os.environ.get("MM_FORGE_LLAMA_CPP"):
        cands.append(Path(os.environ["MM_FORGE_LLAMA_CPP"]) / "convert_hf_to_gguf.py")
    cands.append(Path(__file__).resolve().parents[2] / "engines" / "llama.cpp" / "convert_hf_to_gguf.py")
    for c in cands:
        if c.is_file():
            return c
    raise FileNotFoundError("convert_hf_to_gguf.py not found: give convert_script, or set MM_FORGE_LLAMA_CPP to the "
                            "llama.cpp source folder")


def run_gguf(src_hf: Path, outfile: Path, outtype: str, args: dict, em: Emitter, virtual: bool,
             model_name: str | None = None) -> None:
    """llama.cpp's converter on a Hugging Face checkpoint (through our driver when experts are virtual)."""
    converter = find_converter(args)
    cmd = [sys.executable, "-u"]
    env = dict(os.environ, PYTHONIOENCODING="utf-8", PYTHONPATH=str(Path(__file__).resolve().parents[1]))
    if virtual:
        cmd += ["-m", "mm_forge.dusty_gguf", "--converter", str(converter)]
    else:
        cmd += [str(converter)]
    cmd += [str(src_hf), "--outfile", str(outfile), "--outtype", outtype, "--no-mtp"]
    if model_name:
        cmd += ["--model-name", model_name]
    outfile.parent.mkdir(parents=True, exist_ok=True)
    em.log("converting: " + " ".join(cmd[2:]))
    _stream(cmd, env, em)


def _stream(cmd: list[str], env: dict, em: Emitter) -> None:
    p = subprocess.Popen(cmd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                         text=True, encoding="utf-8", errors="replace")
    tail: list[str] = []
    last_pct = -1
    assert p.stdout is not None
    for line in p.stdout:
        line = line.rstrip()
        if line:
            tail = (tail + [line])[-20:]
            if "Writing:" in line:  # a tqdm bar: report the percentage now and then, not every redraw
                m = re.search(r"Writing:\s+(\d+)%", line)
                if m and int(m.group(1)) != last_pct:
                    last_pct = int(m.group(1))
                    em.progress(last_pct, 100, f"writing GGUF {last_pct}%")
            elif line.startswith(("INFO:", "WARNING", "ERROR")):
                em.log(line[:300], level="info")
    rc = p.wait()
    if rc != 0:
        raise RuntimeError(f"{Path(cmd[2]).name} exited with {rc}:\n" + "\n".join(tail))


def quantize(exe: str, src: Path, dst: Path, qtype: str, em: Emitter) -> None:
    em.log(f"llama-quantize {qtype}")
    _stream([exe, str(src), str(dst), qtype], dict(os.environ), em)


def fix_q8_doubled_scales(gguf_path: Path, src_dir: Path, converter: Path, em: Emitter) -> dict:
    """Makes the shared experts' doubled down projections exact in a Q8_0 GGUF.

    Q8_0 stores 32 weights as int8 plus one float16 scale d = f16(amax / 127). Doubling a weight doubles amax, but
    f16(2 * amax / 127) is not always 2 * f16(amax / 127): where d is tiny (float16 subnormals) the rounding grid is
    absolute, so the rounded value differs, and about half a percent of the blocks of some layers would carry a
    scale that is off by one step. The int8 values are the same in both. So the scales of the doubled tensors are
    rewritten as exactly twice the scale the original weights get, which makes them 2 * Q8_0(W) bit for bit."""
    import numpy as np
    import torch  # noqa: F401  (the Source reads tensors with torch)

    sys.path.insert(0, str(converter.parent / "gguf-py"))
    from gguf import GGUFReader  # type: ignore

    src = Source(src_dir)
    reader = GGUFReader(str(gguf_path), "r+")
    fixed = blocks_changed = 0
    for t in reader.tensors:
        m = re.fullmatch(r"blk\.(\d+)\.ffn_down_shexp\.weight", t.name)
        if not m:
            continue
        if t.tensor_type.name != "Q8_0":
            raise ValueError(f"{t.name} is {t.tensor_type.name}, not Q8_0")
        w = src.get(_source_name(src, f"model.layers.{m.group(1)}.mlp.shared_expert.down_proj.weight"))
        blocks = w.float().numpy().reshape(-1, 32)
        d = (np.abs(blocks).max(axis=1) / np.float32(127)).astype(np.float32)
        want = (d.astype(np.float16).astype(np.float32) * 2).astype(np.float16)
        raw = np.asarray(t.data).reshape(-1, 34)
        if raw.shape[0] != want.shape[0]:
            raise ValueError(f"{t.name}: {raw.shape[0]} blocks, expected {want.shape[0]}")
        have = raw[:, :2].copy().view(np.float16).ravel()
        blocks_changed += int((have != want).sum())
        raw[:, :2] = want.view(np.uint8).reshape(-1, 2)
        fixed += 1
        em.progress(fixed, 32, f"exact doubled scales: layer {m.group(1)}")
    reader.data.flush() if hasattr(reader.data, "flush") else None
    del reader
    return {"tensors": fixed, "blocks_rescaled": blocks_changed}


def run(args: dict, em: Emitter) -> dict:
    """Arguments: source (folder or Hugging Face repo id, default Qwen/Qwen3.5-4B), revision, out_dir, name
    (default Dusty-1.0), slots/top_k/ff_divisor/seed/router_std/expert_init, experts (auto|write|virtual),
    outtypes (e.g. ["bf16"]; also f16, q8_0), quantize (e.g. ["Q8_0"], needs a 16-bit GGUF), quantize_exe,
    convert_script, hf_dir (where the Hugging Face checkpoint goes, default out_dir/hf), skip_hf (reuse it), keep_hf
    (default true)."""
    t0 = time.time()
    data_dir = Path(args.get("data_dir") or os.environ.get("MM_FORGE_DATA") or "forge-data")
    name = args.get("name", "Dusty-1.0")
    out_dir = Path(args.get("out_dir") or data_dir / "dusty" / name)
    cfg = DustyConfig(**{k: args[k] for k in ("slots", "top_k", "ff_divisor", "seed", "router_std", "expert_init",
                                              "expert_std") if k in args})
    source = args.get("source", "Qwen/Qwen3.5-4B")
    src_dir = Path(source)
    if not src_dir.is_dir():
        from huggingface_hub import snapshot_download

        em.log(f"downloading {source}")
        src_dir = Path(snapshot_download(source, revision=args.get("revision"),
                                         local_dir=str(data_dir / "hf" / source.split("/")[-1])))
    src_config = json.loads((src_dir / "config.json").read_text())
    t = text_config(src_config)
    license_ok = (src_dir / "LICENSE").is_file() and "Apache License" in (src_dir / "LICENSE").read_text(errors="ignore")
    cfg.validate(int(t["intermediate_size"]))

    hf_dir = Path(args["hf_dir"]) if args.get("hf_dir") else out_dir / "hf"
    gguf_dir = out_dir / "gguf"
    mode = args.get("experts", "auto")
    outtypes = list(args.get("outtypes", ["bf16"]))
    need = estimate_expert_bytes(t, cfg)
    if mode == "auto":
        mode = "write" if _free(out_dir) > 2 * need + (30 << 30) else "virtual"
    em.log(f"experts: {mode} (routed experts need about {need / 2**30:.1f} GiB in 16 bit)")
    result: dict = {"name": name, "source": source, "source_dir": str(src_dir), "experts": mode,
                    "config": asdict(cfg), "license": "Apache-2.0" if license_ok else "see source LICENSE",
                    "outputs": {}}

    if not args.get("skip_hf"):
        result["hf"] = convert_hf(src_dir, hf_dir, cfg, mode, em)
    else:
        _, info = plan(Source(src_dir), src_config, cfg, False)
        result["hf"] = {"reused": str(hf_dir), "dropped": info["dropped"], "dtype": info["dtype"]}
    result["hf"]["dir"] = str(hf_dir)
    em.artifact(f"{name}-hf", str(hf_dir), kind="hf-checkpoint")

    virtual = (hf_dir / VIRTUAL_FILE).is_file()
    for ot in outtypes:
        out = gguf_dir / f"{name}-{ot.upper()}.gguf"
        run_gguf(hf_dir, out, ot, args, em, virtual, model_name=name)
        result["outputs"][ot] = {"path": str(out), "bytes": out.stat().st_size}
        em.artifact(f"{name}-{ot}", str(out), bytes=out.stat().st_size, kind="gguf")
    for q in args.get("quantize", []):
        base = next((result["outputs"][k]["path"] for k in ("f16", "bf16") if k in result["outputs"]), None)
        exe = args.get("quantize_exe") or os.environ.get("MM_FORGE_LLAMA_QUANTIZE")
        if not base or not exe:
            raise ValueError("quantize needs a 16-bit GGUF in outtypes and quantize_exe (llama-quantize)")
        out = gguf_dir / f"{name}-{q.upper()}.gguf"
        quantize(exe, Path(base), out, q, em)
        result["outputs"][q.lower()] = {"path": str(out), "bytes": out.stat().st_size}
        em.artifact(f"{name}-{q.lower()}", str(out), bytes=out.stat().st_size, kind="gguf")
    for k, o in result["outputs"].items():
        if k.lower() == "q8_0":
            o["doubled_scales"] = fix_q8_doubled_scales(Path(o["path"]), src_dir, find_converter(args), em)
            o["bytes"] = Path(o["path"]).stat().st_size
    if args.get("sha256", True):
        for k, o in result["outputs"].items():
            o["sha256"] = file_sha256(o["path"])

    if not args.get("keep_hf", True):
        shutil.rmtree(hf_dir, ignore_errors=True)
    result["dropped"] = result["hf"].get("dropped", {})
    result["elapsed_s"] = round(time.time() - t0, 1)
    (out_dir / "dusty.json").write_text(json.dumps(result, indent=2))
    return result
