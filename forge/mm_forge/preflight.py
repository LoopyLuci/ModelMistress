"""The hardware guard: proves that an optimizer step really runs on the GPU before a long job starts.

It builds a tiny transformer, runs the same seeded training steps on the CPU and on the GPU (forward, backward,
AdamW step) and compares the two: the losses, the gradients and the updates must agree. It also checks bf16 (autocast
training, and a matrix product against fp32), allocates and fills a block of GPU memory, and measures matrix-product
speed. If no GPU is usable the same steps run on the CPU and the answer says so plainly (`device_kind: "cpu"`,
`ok: false`).
"""

from __future__ import annotations

import platform
import sys
import time

from .protocol import Emitter
from .seeding import seed_everything

VOCAB, DIM, HEADS, SEQ, BATCH = 512, 128, 4, 64, 8


def _build(torch, nn, seed: int):
    """A small causal transformer (embedding, two blocks with attention and an MLP, tied output)."""
    F = torch.nn.functional

    class Block(nn.Module):
        def __init__(self):
            super().__init__()
            self.n1, self.n2 = nn.LayerNorm(DIM), nn.LayerNorm(DIM)
            self.qkv, self.proj = nn.Linear(DIM, 3 * DIM), nn.Linear(DIM, DIM)
            self.up, self.down = nn.Linear(DIM, 2 * DIM), nn.Linear(2 * DIM, DIM)

        def forward(self, x):
            b, t, _ = x.shape
            q, k, v = self.qkv(self.n1(x)).view(b, t, 3, HEADS, DIM // HEADS).permute(2, 0, 3, 1, 4)
            a = F.scaled_dot_product_attention(q, k, v, is_causal=True).transpose(1, 2).reshape(b, t, DIM)
            x = x + self.proj(a)
            return x + self.down(F.gelu(self.up(self.n2(x))))

    class Tiny(nn.Module):
        def __init__(self):
            super().__init__()
            self.emb = nn.Embedding(VOCAB, DIM)
            self.blocks = nn.ModuleList([Block(), Block()])
            self.norm = nn.LayerNorm(DIM)
            nn.init.normal_(self.emb.weight, std=0.02)  # a sane starting loss (about ln(vocab)) with the tied output

        def forward(self, ids):
            x = self.emb(ids)
            for b in self.blocks:
                x = b(x)
            return self.norm(x) @ self.emb.weight.t()

    torch.manual_seed(seed)
    return Tiny()


def _flat(torch, tensors):
    return torch.cat([t.detach().float().reshape(-1).cpu() for t in tensors])


def _train(torch, model, device: str, batches, steps: int, lr: float, autocast_bf16: bool = False):
    model = model.to(device).train()
    opt = torch.optim.AdamW(model.parameters(), lr=lr)
    before = _flat(torch, model.parameters())
    losses, first_grads = [], None
    for i in range(steps):
        ids = batches[i].to(device)
        opt.zero_grad(set_to_none=True)
        with torch.autocast(device_type=device.split(":")[0], dtype=torch.bfloat16, enabled=autocast_bf16):
            logits = model(ids[:, :-1])
            loss = torch.nn.functional.cross_entropy(logits.float().reshape(-1, VOCAB), ids[:, 1:].reshape(-1))
        loss.backward()
        if first_grads is None:
            first_grads = _flat(torch, [p.grad for p in model.parameters()])
        opt.step()
        losses.append(float(loss.item()))
    if device.startswith("cuda"):
        torch.cuda.synchronize()
    after = _flat(torch, model.parameters())
    return {"losses": losses, "grads": first_grads, "update": after - before, "finite": bool(after.isfinite().all())}


def _cos(a, b) -> float:
    d = a.norm() * b.norm()
    return float((a @ b) / d) if float(d) > 0 else 0.0


def _rel(a, b) -> float:
    d = b.norm()
    return float((a - b).norm() / d) if float(d) > 0 else float((a - b).norm())


def run(args: dict, em: Emitter) -> dict:
    t0 = time.time()
    seed = int(args.get("seed", 1234))
    steps = int(args.get("steps", 5))
    result: dict = {
        "ok": False,
        "python": sys.version.split()[0],
        "platform": platform.platform(),
        "checks": {},
    }
    try:
        import torch
        from torch import nn
    except Exception as e:  # noqa: BLE001
        result["error"] = f"cannot import torch: {e}"
        result["device_kind"] = "none"
        return result

    seed_everything(seed, deterministic=False)
    result.update(torch=torch.__version__, hip=getattr(torch.version, "hip", None), cuda=torch.version.cuda)
    has_gpu = bool(torch.cuda.is_available())
    result["gpu_available"] = has_gpu
    em.log(f"torch {torch.__version__}, hip {result['hip']}, gpu available: {has_gpu}")

    # identical weights and data for every run
    base = _build(torch, nn, seed)
    state = {k: v.clone() for k, v in base.state_dict().items()}
    gen = torch.Generator().manual_seed(seed + 1)
    batches = [torch.randint(0, VOCAB, (BATCH, SEQ + 1), generator=gen) for _ in range(steps)]

    def fresh():
        m = _build(torch, nn, seed)
        m.load_state_dict(state)
        return m

    checks = result["checks"]
    em.progress(0, 5, "optimizer step on the CPU")
    cpu = _train(torch, fresh(), "cpu", batches, steps, 1e-3)
    checks["cpu_reference"] = {"ok": cpu["finite"], "losses": cpu["losses"]}

    if not has_gpu:
        result["device_kind"] = "cpu"
        result["message"] = "no usable GPU: the optimizer step ran on the CPU only (slow); GPU jobs will refuse to start"
        result["elapsed_s"] = round(time.time() - t0, 2)
        return result

    dev = "cuda:0"
    props = torch.cuda.get_device_properties(0)
    free, total = torch.cuda.mem_get_info(0)
    result.update(
        device_kind="gpu",
        device=dev,
        device_name=torch.cuda.get_device_name(0),
        arch=getattr(props, "gcnArchName", None),
        vram_total_bytes=int(total),
        vram_free_bytes=int(free),
        compute_units=props.multi_processor_count,
    )
    try:
        result["bf16_supported"] = bool(torch.cuda.is_bf16_supported())
    except Exception:  # noqa: BLE001
        result["bf16_supported"] = False

    em.progress(1, 5, "optimizer step on the GPU")
    try:
        gpu = _train(torch, fresh(), dev, batches, steps, 1e-3)
        loss_diff = max(abs(a - b) / max(abs(b), 1e-9) for a, b in zip(gpu["losses"], cpu["losses"]))
        grad_rel = _rel(gpu["grads"], cpu["grads"])
        upd_cos = _cos(gpu["update"], cpu["update"])
        moved = float(gpu["update"].norm())
        ok = gpu["finite"] and loss_diff < 1e-3 and grad_rel < 1e-2 and upd_cos > 0.9 and moved > 0
        checks["optimizer_step_gpu_vs_cpu"] = {
            "ok": ok,
            "steps": steps,
            "gpu_losses": gpu["losses"],
            "cpu_losses": cpu["losses"],
            "max_loss_rel_diff": loss_diff,
            "grad_rel_err": grad_rel,
            "update_cosine": upd_cos,
            "update_norm": moved,
        }
        for i, l in enumerate(gpu["losses"]):
            em.metric("preflight_loss_gpu", l, step=i)
    except Exception as e:  # noqa: BLE001
        checks["optimizer_step_gpu_vs_cpu"] = {"ok": False, "error": f"{type(e).__name__}: {e}"}

    em.progress(2, 5, "bf16")
    try:
        bf = _train(torch, fresh(), dev, batches, steps, 1e-3, autocast_bf16=True)
        a = torch.randn(512, 512, device=dev)
        b = torch.randn(512, 512, device=dev)
        ref = a @ b
        err = float((a.bfloat16() @ b.bfloat16()).float().sub(ref).norm() / ref.norm())
        # bf16 training follows the fp32 reference closely (bf16 keeps about 3 significant digits)
        loss_rel = max(abs(x - y) / max(abs(y), 1e-9) for x, y in zip(bf["losses"], cpu["losses"]))
        checks["bf16"] = {
            "ok": bf["finite"] and err < 5e-2 and loss_rel < 2e-2,
            "autocast_losses": bf["losses"],
            "max_loss_rel_diff_vs_fp32": loss_rel,
            "matmul_rel_err": err,
        }
    except Exception as e:  # noqa: BLE001
        checks["bf16"] = {"ok": False, "error": f"{type(e).__name__}: {e}"}

    em.progress(3, 5, "memory")
    try:
        gib = min(float(args.get("memory_gib", 4)), max(0.25, free / 2**30 - 1))
        x = torch.empty(int(gib * 2**30) // 4, dtype=torch.float32, device=dev)
        x.fill_(1.0)
        probe = x[:: max(1, x.numel() // 1_000_000)]
        total_probe, n_probe = float(probe.sum()), probe.numel()
        peak = torch.cuda.max_memory_allocated(0)
        del x, probe
        torch.cuda.empty_cache()
        checks["memory"] = {"ok": abs(total_probe - n_probe) < 1e-3, "allocated_gib": round(gib, 2), "peak_bytes": int(peak)}
    except Exception as e:  # noqa: BLE001
        checks["memory"] = {"ok": False, "error": f"{type(e).__name__}: {e}"}

    em.progress(4, 5, "speed")
    try:
        n = 4096
        x = torch.randn(n, n, device=dev, dtype=torch.bfloat16)
        for _ in range(3):
            x @ x
        torch.cuda.synchronize()
        t = time.time()
        iters = 20
        for _ in range(iters):
            y = x @ x
        torch.cuda.synchronize()
        dt = time.time() - t
        tflops = 2 * n**3 * iters / dt / 1e12
        checks["matmul_bf16"] = {"ok": bool(y.isfinite().all()), "tflops": round(tflops, 2)}
        em.metric("preflight_bf16_tflops", tflops)
    except Exception as e:  # noqa: BLE001
        checks["matmul_bf16"] = {"ok": False, "error": f"{type(e).__name__}: {e}"}

    result["ok"] = all(c.get("ok") for c in checks.values())
    result["elapsed_s"] = round(time.time() - t0, 2)
    em.progress(5, 5, "done")
    return result
