"""The Dusty conversion's tensor mapping, on a tiny synthetic checkpoint shaped like Qwen3.5-4B (no download)."""

import json

import pytest

torch = pytest.importorskip("torch")
from safetensors import safe_open  # noqa: E402
from safetensors.torch import save_file  # noqa: E402

from mm_forge import dusty  # noqa: E402
from mm_forge.dusty import DustyConfig, classify  # noqa: E402
from mm_forge.protocol import Emitter  # noqa: E402

LAYERS, HIDDEN, INTER, VOCAB = 4, 32, 64, 50
BF = torch.bfloat16


def make_source(path):
    g = torch.Generator().manual_seed(0)

    def r(*shape):
        return (torch.randn(*shape, generator=g) * 0.5).to(BF)

    t = {"model.language_model.embed_tokens.weight": r(VOCAB, HIDDEN),
         "model.language_model.norm.weight": r(HIDDEN),
         "model.visual.blocks.0.attn.qkv.weight": r(6, 6),
         "model.visual.merger.norm.bias": r(6),
         "mtp.fc.weight": r(HIDDEN, 2 * HIDDEN),
         "mtp.layers.0.mlp.down_proj.weight": r(HIDDEN, INTER)}
    for l in range(LAYERS):
        p = f"model.language_model.layers.{l}"
        t[f"{p}.input_layernorm.weight"] = r(HIDDEN)
        t[f"{p}.post_attention_layernorm.weight"] = r(HIDDEN)
        t[f"{p}.mlp.gate_proj.weight"] = r(INTER, HIDDEN)
        t[f"{p}.mlp.up_proj.weight"] = r(INTER, HIDDEN)
        t[f"{p}.mlp.down_proj.weight"] = r(HIDDEN, INTER)
        if l % 4 == 3:
            t[f"{p}.self_attn.q_proj.weight"] = r(8, HIDDEN)
        else:
            t[f"{p}.linear_attn.A_log"] = r(4)
    half = sorted(t)[: len(t) // 2]
    save_file({k: t[k] for k in half}, str(path / "model.safetensors-00001-of-00002.safetensors"))
    save_file({k: v for k, v in t.items() if k not in half}, str(path / "model.safetensors-00002-of-00002.safetensors"))
    wm = {k: "model.safetensors-00001-of-00002.safetensors" if k in half else "model.safetensors-00002-of-00002.safetensors"
          for k in t}
    (path / "model.safetensors.index.json").write_text(json.dumps({"metadata": {}, "weight_map": wm}))
    (path / "config.json").write_text(json.dumps({
        "architectures": ["Qwen3_5ForConditionalGeneration"], "model_type": "qwen3_5",
        "text_config": {"hidden_size": HIDDEN, "intermediate_size": INTER, "num_hidden_layers": LAYERS,
                        "vocab_size": VOCAB, "model_type": "qwen3_5_text", "mtp_num_hidden_layers": 1,
                        "tie_word_embeddings": True, "rms_norm_eps": 1e-6},
        "vision_config": {"depth": 1}}))
    (path / "tokenizer.json").write_text("{}")
    return t


@pytest.fixture()
def source(tmp_path):
    d = tmp_path / "src"
    d.mkdir()
    return d, make_source(d)


def em():
    import io

    return Emitter(io.StringIO())


def read_all(d):
    out = {}
    for f in sorted(d.glob("model-*.safetensors")):
        with safe_open(str(f), framework="pt") as h:
            for k in h.keys():
                out[k] = h.get_tensor(k)
    return out


def test_classify_names():
    assert classify("model.language_model.layers.3.mlp.down_proj.weight") == ("double", "model.layers.3.mlp.shared_expert.down_proj.weight")
    assert classify("model.language_model.layers.3.mlp.gate_proj.weight")[0] == "ffn"
    assert classify("model.language_model.layers.3.mlp.up_proj.weight")[1] == "model.layers.3.mlp.shared_expert.up_proj.weight"
    assert classify("model.language_model.layers.0.linear_attn.A_log") == ("keep", "model.layers.0.linear_attn.A_log")
    assert classify("model.language_model.embed_tokens.weight") == ("keep", "model.embed_tokens.weight")
    assert classify("model.language_model.norm.weight") == ("keep", "model.norm.weight")
    assert classify("model.visual.blocks.0.attn.qkv.weight") == ("drop", "vision tower")
    assert classify("mtp.layers.0.mlp.down_proj.weight")[0] == "drop"
    assert classify("model.layers.1.mlp.gate_proj.weight")[1] == "model.layers.1.mlp.shared_expert.gate_proj.weight"


def test_config_is_the_qwen35_moe_text_model(source):
    d, _ = source
    cfg = dusty.build_config(json.loads((d / "config.json").read_text()), DustyConfig())
    assert cfg["architectures"] == ["Qwen3_5MoeForCausalLM"] and cfg["model_type"] == "qwen3_5_moe_text"
    assert (cfg["num_experts"], cfg["num_experts_per_tok"]) == (64, 4)
    assert (cfg["moe_intermediate_size"], cfg["shared_expert_intermediate_size"]) == (INTER // 8, INTER)
    assert cfg["mtp_num_hidden_layers"] == 0 and cfg["norm_topk_prob"] is True and "text_config" not in cfg
    assert cfg["hidden_size"] == HIDDEN and cfg["tie_word_embeddings"] is True
    with pytest.raises(ValueError):
        dusty.build_config({"intermediate_size": 60, "hidden_size": 8}, DustyConfig())  # 60 is not divisible by 8


def test_the_converted_checkpoint_has_exactly_the_planned_tensors(source, tmp_path):
    d, src = source
    cfg = DustyConfig(slots=16, top_k=4)
    info = dusty.convert_hf(d, tmp_path / "hf", cfg, "write", em())
    got = read_all(tmp_path / "hf")
    ie = INTER // 8
    expected = {"model.embed_tokens.weight", "model.norm.weight"}
    for l in range(LAYERS):
        p = f"model.layers.{l}"
        expected |= {f"{p}.input_layernorm.weight", f"{p}.post_attention_layernorm.weight",
                     f"{p}.mlp.shared_expert.gate_proj.weight", f"{p}.mlp.shared_expert.up_proj.weight",
                     f"{p}.mlp.shared_expert.down_proj.weight", f"{p}.mlp.shared_expert_gate.weight",
                     f"{p}.mlp.gate.weight", f"{p}.mlp.experts.gate_up_proj", f"{p}.mlp.experts.down_proj"}
        expected.add(f"{p}.self_attn.q_proj.weight" if l == 3 else f"{p}.linear_attn.A_log")
    assert set(got) == expected
    assert got["model.layers.0.mlp.experts.gate_up_proj"].shape == (16, 2 * ie, HIDDEN)
    assert got["model.layers.0.mlp.experts.down_proj"].shape == (16, HIDDEN, ie)
    assert got["model.layers.0.mlp.gate.weight"].shape == (16, HIDDEN)
    assert got["model.layers.0.mlp.shared_expert_gate.weight"].shape == (1, HIDDEN)
    assert set(info["dropped"]) == {"vision tower", "multi-token prediction head"}
    assert info["dropped"]["vision tower"]["tensors"] == 2 and info["dropped"]["multi-token prediction head"]["tensors"] == 2
    assert info["dropped"]["vision tower"]["bytes"] == (36 + 6) * 2
    cfg_json = json.loads((tmp_path / "hf" / "config.json").read_text())
    assert cfg_json["num_experts"] == 16 and (tmp_path / "hf" / "tokenizer.json").is_file()


def test_shared_expert_is_the_dense_ffn_with_gate_zero_and_down_doubled(source, tmp_path):
    d, src = source
    dusty.convert_hf(d, tmp_path / "hf", DustyConfig(slots=8, top_k=2), "write", em())
    got = read_all(tmp_path / "hf")
    for l in range(LAYERS):
        p, q = f"model.layers.{l}.mlp", f"model.language_model.layers.{l}.mlp"
        assert torch.equal(got[f"{p}.shared_expert.gate_proj.weight"], src[f"{q}.gate_proj.weight"])
        assert torch.equal(got[f"{p}.shared_expert.up_proj.weight"], src[f"{q}.up_proj.weight"])
        assert torch.equal(got[f"{p}.shared_expert.down_proj.weight"], src[f"{q}.down_proj.weight"] * 2)
        assert not got[f"{p}.shared_expert_gate.weight"].any()
        assert not got[f"{p}.experts.down_proj"].any(), "routed experts add exactly nothing"
        assert got[f"{p}.gate.weight"].any() and got[f"{p}.gate.weight"].float().std() < 0.05


def test_routed_gate_up_are_slices_of_the_dense_ffn_and_routers_are_seeded(source, tmp_path):
    d, src = source
    cfg = DustyConfig(slots=16, top_k=4, seed=5)
    dusty.convert_hf(d, tmp_path / "a", cfg, "write", em())
    dusty.convert_hf(d, tmp_path / "b", DustyConfig(slots=16, top_k=4, seed=5), "write", em())
    dusty.convert_hf(d, tmp_path / "c", DustyConfig(slots=16, top_k=4, seed=6), "write", em())
    a, b, c = read_all(tmp_path / "a"), read_all(tmp_path / "b"), read_all(tmp_path / "c")
    assert all(torch.equal(a[k], b[k]) for k in a), "same seed, same checkpoint"
    assert not torch.equal(a["model.layers.0.mlp.gate.weight"], c["model.layers.0.mlp.gate.weight"])
    assert torch.equal(a["model.layers.0.mlp.shared_expert.up_proj.weight"], c["model.layers.0.mlp.shared_expert.up_proj.weight"])
    assert not torch.equal(a["model.layers.0.mlp.gate.weight"], a["model.layers.1.mlp.gate.weight"])
    ie = INTER // 8
    gu = a["model.layers.2.mlp.experts.gate_up_proj"]
    gate = src["model.language_model.layers.2.mlp.gate_proj.weight"]
    up = src["model.language_model.layers.2.mlp.up_proj.weight"]
    for s in (0, 3, 9, 15):
        j = s % 8
        assert torch.equal(gu[s, :ie], gate[j * ie:(j + 1) * ie]) and torch.equal(gu[s, ie:], up[j * ie:(j + 1) * ie])


def test_virtual_experts_are_the_same_tensors_as_written_ones(source, tmp_path):
    d, _ = source
    cfg = DustyConfig(slots=16, top_k=4)
    dusty.convert_hf(d, tmp_path / "w", cfg, "write", em())
    dusty.convert_hf(d, tmp_path / "v", cfg, "virtual", em())
    w, v = read_all(tmp_path / "w"), read_all(tmp_path / "v")
    assert not any("experts." in k for k in v) and set(w) - set(v) == {
        f"model.layers.{l}.mlp.experts.{n}" for l in range(LAYERS) for n in ("gate_up_proj", "down_proj")}
    assert (tmp_path / "v" / dusty.VIRTUAL_FILE).is_file()
    for spec in dusty.virtual_specs(tmp_path / "v"):
        made = spec.make() if spec.make else torch.zeros(spec.shape, dtype=BF)
        assert made.shape == w[spec.name].shape and torch.equal(made, w[spec.name]), spec.name


def moe_block(x, t, p, top_k):
    """The Hugging Face Qwen3-Next sparse MoE block (softmax router, top-k weights normalised, sigmoid-gated shared
    expert), in float32, on the converted tensors."""
    f = lambda n: t[f"{p}.{n}"].float()
    silu = torch.nn.functional.silu
    probs = torch.softmax(x @ f("gate.weight").t(), dim=-1)
    w, idx = torch.topk(probs, top_k, dim=-1)
    w = w / w.sum(-1, keepdim=True)
    out = torch.zeros_like(x)
    gu, down = f("experts.gate_up_proj"), f("experts.down_proj")
    ie = down.shape[-1]
    for tok in range(x.shape[0]):
        for k in range(top_k):
            e = int(idx[tok, k])
            h = silu(gu[e, :ie] @ x[tok]) * (gu[e, ie:] @ x[tok])
            out[tok] += w[tok, k] * (down[e] @ h)
    shared = (silu(x @ f("shared_expert.gate_proj.weight").t()) * (x @ f("shared_expert.up_proj.weight").t())) \
        @ f("shared_expert.down_proj.weight").t()
    return out + torch.sigmoid(x @ f("shared_expert_gate.weight").t()) * shared


def test_the_moe_layer_reproduces_the_dense_ffn_exactly(source, tmp_path):
    d, src = source
    dusty.convert_hf(d, tmp_path / "hf", DustyConfig(slots=16, top_k=4), "write", em())
    t = read_all(tmp_path / "hf")
    x = torch.randn(7, HIDDEN, generator=torch.Generator().manual_seed(3))
    for l in range(LAYERS):
        q = f"model.language_model.layers.{l}.mlp"
        g, u, dn = (src[f"{q}.{n}_proj.weight"].float() for n in ("gate", "up", "down"))
        dense = (torch.nn.functional.silu(x @ g.t()) * (x @ u.t())) @ dn.t()
        assert torch.equal(moe_block(x, t, f"model.layers.{l}.mlp", 4), dense), f"layer {l}"


def test_doubling_refuses_an_overflow():
    big = torch.tensor([3e38], dtype=torch.float32)
    with pytest.raises(ValueError):
        dusty.double(torch, big)
    ok = torch.tensor([1.5, -0.25, 3.0e-39], dtype=torch.float32)
    assert torch.equal(dusty.double(torch, ok), ok * 2)
