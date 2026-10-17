"""Fine-tuning: datasets, configuration, VRAM planning, checkpoints, and a tiny end-to-end LoRA run on the CPU with
a random Llama and a tokenizer built in the test (no download)."""

import json

import pytest

from mm_forge import steps, traindata
from mm_forge import train as T
from mm_forge.protocol import Emitter, read_events


# ---- datasets (no torch needed) ----------------------------------------------------------------------------------

def test_formats_alpaca_sharegpt_chat(tmp_path):
    f = tmp_path / "d.jsonl"
    rows = [
        {"instruction": "Add", "input": "1+1", "output": "2"},
        {"conversations": [{"from": "human", "value": "hi"}, {"from": "gpt", "value": "hello"}]},
        {"messages": [{"role": "system", "content": "s"}, {"role": "user", "content": "q"},
                      {"role": "assistant", "content": "a"}]},
        {"prompt": "p", "completion": "c"},
        {"nothing": 1},
        {"messages": [{"role": "user", "content": "no answer"}]},
    ]
    f.write_text("\n".join(json.dumps(r) for r in rows) + "\n\n")
    ex, st = traindata.load_examples(str(f))
    assert st["rows"] == 6 and st["usable"] == 4 and st["skipped"] == 2
    assert st["formats"] == {"alpaca": 1, "sharegpt": 1, "chat": 1, "prompt_completion": 1}
    assert ex[0] == [{"role": "user", "content": "Add\n\n1+1"}, {"role": "assistant", "content": "2"}]
    assert ex[1] == [{"role": "user", "content": "hi"}, {"role": "assistant", "content": "hello"}]
    assert [m["role"] for m in ex[2]] == ["system", "user", "assistant"]


def test_json_list_and_bad_line(tmp_path):
    a = tmp_path / "a.json"
    a.write_text(json.dumps({"data": [{"instruction": "i", "output": "o"}]}))
    assert traindata.load_examples([str(a)])[1]["usable"] == 1
    b = tmp_path / "b.jsonl"
    b.write_text('{"text": "x"}\nnot json\n')
    with pytest.raises(ValueError, match="b.jsonl:2"):
        traindata.load_rows(str(b))


def test_split_is_seeded_and_keeps_training_rows():
    ex = list(range(10))
    tr, ev = traindata.split(ex, 0.2, seed=1)
    assert len(tr) == 8 and len(ev) == 2 and sorted(tr + ev) == ex
    assert traindata.split(ex, 0.2, seed=1) == (tr, ev)
    tr, ev = traindata.split([1], 0.9)
    assert tr == [1] and ev == []
    assert traindata.split(ex, 0.0)[1] == []


def test_batch_order_deterministic_and_complete():
    o = traindata.batch_order(23, 4, 5, list(range(23)))
    assert o == traindata.batch_order(23, 4, 5, list(range(23)))
    assert sorted(i for b in o for i in b) == list(range(23))
    assert o != traindata.batch_order(23, 4, 6, list(range(23)))


# ---- configuration -----------------------------------------------------------------------------------------------

def test_config_from_args_and_errors():
    c = T.TrainConfig.from_args({"base_model": "m", "data": "d.json", "out_dir": "o", "rank": 8, "epochs": 2})
    assert c.data == ["d.json"] and c.rank == 8 and c.load_in_4bit is False
    assert T.TrainConfig.from_args({"model_path": "m", "data": ["a", "b"], "out_dir": "o"}, qlora=True).load_in_4bit
    for bad in ({"data": "d", "out_dir": "o"}, {"base_model": "m", "out_dir": "o"}, {"base_model": "m", "data": "d"},
                {"base_model": "m", "data": "d", "out_dir": "o", "bogus": 1},
                {"base_model": "m", "data": "d", "out_dir": "o", "rank": 0},
                {"base_model": "m", "data": "d", "out_dir": "o", "device": "tpu"}):
        with pytest.raises(T.TrainError):
            T.TrainConfig.from_args(bad)


def test_schedule():
    c = T.TrainConfig.from_args({"base_model": "m", "data": "d", "out_dir": "o", "batch_size": 2, "grad_accum": 3,
                                 "epochs": 2})
    assert T.total_steps(c, 13) == 6  # ceil(13/2/3) = 3 per epoch
    c.max_steps = 5
    assert T.total_steps(c, 13) == 5
    assert T.lr_at(0, 100, 1.0, 10) == pytest.approx(0.1)
    assert T.lr_at(9, 100, 1.0, 10) == pytest.approx(1.0)
    assert T.lr_at(100, 100, 1.0, 10) == pytest.approx(0.0, abs=1e-9)


# ---- VRAM --------------------------------------------------------------------------------------------------------

DIMS = {"weights_bytes": 3 << 30, "hidden": 2048, "layers": 28, "intermediate": 6144, "vocab": 150000}
GIB = 1 << 30


def _cfg(**kw):
    return T.TrainConfig.from_args({"base_model": "m", "data": "d", "out_dir": "o", "max_seq_length": 1024, **kw})


def test_plan_fits_as_asked():
    p = T.plan_memory(DIMS, _cfg(batch_size=2, grad_accum=4), 24 * GIB, False)
    assert (p["batch_size"], p["grad_accum"], p["changed"]) == (2, 4, False)


def test_plan_shrinks_batch_keeping_effective_batch():
    cfg = _cfg(batch_size=8, grad_accum=1)
    need8 = T.estimate_bytes(DIMS, 8, 1024, 16, False)
    p = T.plan_memory(DIMS, cfg, int(need8 * 0.6), False)
    assert p["changed"] and p["batch_size"] < 8 and p["batch_size"] * p["grad_accum"] >= 8


def test_plan_picks_4bit_when_16bit_cannot_fit():
    dims = dict(DIMS, weights_bytes=14 * GIB)
    free = 12 * GIB
    with pytest.raises(T.TrainError, match="not enough free VRAM"):
        T.plan_memory(dims, _cfg(), free, False)
    p = T.plan_memory(dims, _cfg(), free, True)
    assert p["load_in_4bit"] is True and p["changed"]


def test_plan_errors_clearly_and_cpu_accepts():
    with pytest.raises(T.TrainError, match=r"1\.0 GiB free"):
        T.plan_memory(DIMS, _cfg(), 1 * GIB, False)
    assert T.plan_memory(DIMS, _cfg(), None, False)["changed"] is False


def test_model_dims_reads_config(tmp_path):
    (tmp_path / "config.json").write_text(json.dumps({"hidden_size": 64, "num_hidden_layers": 3, "vocab_size": 99}))
    (tmp_path / "model.safetensors").write_bytes(b"x" * 1000)
    d = T.model_dims(tmp_path)
    assert (d["hidden"], d["layers"], d["vocab"], d["weights_bytes"]) == (64, 3, 99, 1000)


# ---- steps registered, export wiring -----------------------------------------------------------------------------

def test_steps_registered():
    for n in ("lora_train", "qlora_train", "merge_lora", "lora_to_gguf", "merged_to_gguf"):
        assert callable(steps.load(n).run)
    assert steps.load("qlora_train").run is T.run_qlora


def test_gguf_plan_and_export_errors(tmp_path):
    from mm_forge import export

    plan = export.gguf_plan({"outtypes": ["bf16"], "quantize": ["q4_k_m"]}, "m", tmp_path)
    assert [k for k, _ in plan] == ["bf16", "q4_k_m"] and plan[1][1].name == "m-Q4_K_M.gguf"
    em = Emitter(open(tmp_path / "ev", "w"))
    with pytest.raises(ValueError, match="quantize_exe"):
        export.run_merged_to_gguf({"model_dir": str(tmp_path), "quantize": ["Q8_0"]}, em)
    with pytest.raises(ValueError, match="16-bit"):
        export.run_merged_to_gguf({"model_dir": str(tmp_path), "quantize": ["Q8_0"], "quantize_exe": "x",
                                   "outtypes": ["q8_0"]}, em)
    with pytest.raises(FileNotFoundError, match="config.json"):
        export.run_merged_to_gguf({"model_dir": str(tmp_path)}, em)


def test_merged_to_gguf_calls_converter_and_quantizer(tmp_path, monkeypatch):
    from mm_forge import dusty, export

    model = tmp_path / "merged"
    model.mkdir()
    (model / "config.json").write_text("{}")
    calls = []

    def fake_gguf(src, out, ot, args, em, virtual, model_name=None):
        calls.append(("convert", ot, virtual, model_name))
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(b"GGUF")

    def fake_quant(exe, src, dst, q, em):
        calls.append(("quant", exe, q))
        dst.write_bytes(b"GGUFq")

    monkeypatch.setattr(dusty, "run_gguf", fake_gguf)
    monkeypatch.setattr(dusty, "quantize", fake_quant)
    em = Emitter(open(tmp_path / "ev", "w"))
    r = export.run_merged_to_gguf({"model_dir": str(model), "out_dir": str(tmp_path / "g"), "name": "m",
                                   "quantize": ["Q4_K_M"], "quantize_exe": "lq"}, em)
    assert calls == [("convert", "bf16", False, "m"), ("quant", "lq", "Q4_K_M")]
    assert set(r["outputs"]) == {"bf16", "q4_k_m"} and r["outputs"]["q4_k_m"]["bytes"] == 5


# ---- the real thing, tiny, on the CPU ----------------------------------------------------------------------------

torch = pytest.importorskip("torch")
transformers = pytest.importorskip("transformers")
pytest.importorskip("peft")


def test_checkpoint_roundtrip_and_pruning(tmp_path):
    p = {"w": torch.arange(4.0)}
    for s in (1, 2, 3):
        T.save_train_checkpoint(tmp_path, s, p, {"state": {}, "param_groups": []}, {"epoch": 0}, keep=2)
    assert [s for s, _ in T.list_checkpoints(tmp_path)] == [2, 3]
    step, path = T.find_resume(tmp_path)
    assert step == 3
    tensors, ostate, meta = T.load_train_checkpoint(path)
    assert torch.equal(tensors["w"], p["w"]) and meta["step"] == 3
    del tensors, ostate  # Windows will not overwrite a file that is still mapped
    import gc

    gc.collect()
    # corrupt the newest: resume falls back to the older one
    (path / "weights.safetensors").write_bytes(b"junk")
    assert T.find_resume(tmp_path)[0] == 2
    (T.list_checkpoints(tmp_path)[0][1] / "optim.pt").write_bytes(b"junk")
    assert T.find_resume(tmp_path) is None


@pytest.fixture(scope="module")
def tiny(tmp_path_factory):
    from tokenizers import Tokenizer, models, pre_tokenizers, trainers

    d = tmp_path_factory.mktemp("tiny")
    t = Tokenizer(models.BPE(unk_token="<unk>"))
    t.pre_tokenizer = pre_tokenizers.ByteLevel(add_prefix_space=False)
    t.train_from_iterator(
        ["hello world the quick brown fox 1 2 3 add subtract answer question"] * 5,
        trainers.BpeTrainer(vocab_size=300, special_tokens=["<unk>", "<eos>", "<|im_start|>", "<|im_end|>"],
                            initial_alphabet=pre_tokenizers.ByteLevel.alphabet()))
    tok = transformers.PreTrainedTokenizerFast(tokenizer_object=t, unk_token="<unk>", eos_token="<eos>")
    tok.save_pretrained(d)
    torch.manual_seed(0)
    cfg = transformers.LlamaConfig(vocab_size=len(tok), hidden_size=32, intermediate_size=64, num_hidden_layers=2,
                                   num_attention_heads=4, num_key_value_heads=2, eos_token_id=1)
    transformers.LlamaForCausalLM(cfg).save_pretrained(d)
    rows = [{"instruction": "add", "input": f"{i} {i}", "output": f"answer {i}"} for i in range(1, 9)]
    (d / "data.jsonl").write_text("\n".join(json.dumps(r) for r in rows))
    return d


def _run(args, tmp_path, qlora=False):
    buf = open(tmp_path / "events.jsonl", "w+")
    em = Emitter(buf)
    result = T.run(args, em, qlora=qlora)
    buf.flush()
    buf.seek(0)
    return result, read_events(buf)


def _args(tiny, out, **kw):
    return {"base_model": str(tiny), "data": str(tiny / "data.jsonl"), "out_dir": str(out), "device": "cpu",
            "rank": 4, "alpha": 8, "batch_size": 2, "grad_accum": 2, "max_steps": 4, "save_every": 2,
            "max_seq_length": 48, "learning_rate": 5e-3, "eval_fraction": 0.25, "gradient_checkpointing": False,
            **kw}


def test_tiny_lora_end_to_end_with_metrics_and_resume(tiny, tmp_path):
    out = tmp_path / "run"
    result, events = _run(_args(tiny, out), tmp_path)
    assert result["steps"] == 4 and result["resumed_from"] is None and result["load_in_4bit"] is False
    assert (out / "adapter" / "adapter_config.json").is_file() and (out / "train.json").is_file()
    losses = [e for e in events if e["type"] == "metric" and e["name"] == "loss"]
    assert [e["step"] for e in losses] == [1, 2, 3, 4]
    assert all(e["value"] == e["value"] for e in losses)
    assert {e["name"] for e in events if e["type"] == "metric"} >= {"loss", "lr", "grad_norm", "eval_loss"} - {"eval_loss"}
    assert any(e["type"] == "metric" and e["name"] == "eval_loss" for e in events)
    assert any(e["type"] == "artifact" and e["kind"] == "lora-adapter" for e in events)
    assert T.find_resume(out)[0] == 4

    # more steps from the same out_dir: continues at step 4, runs only the new ones
    result2, events2 = _run(_args(tiny, out, max_steps=6), tmp_path)
    assert result2["resumed_from"] == 4 and result2["steps"] == 6 and result2["steps_this_run"] == 2
    assert [e["step"] for e in events2 if e["type"] == "metric" and e["name"] == "loss"] == [5, 6]


def test_resume_matches_uninterrupted_run(tiny, tmp_path):
    _run(_args(tiny, tmp_path / "full", max_steps=4, eval_fraction=0.0), tmp_path)
    _run(_args(tiny, tmp_path / "split", max_steps=2, eval_fraction=0.0), tmp_path)
    _run(_args(tiny, tmp_path / "split", max_steps=4, eval_fraction=0.0), tmp_path)
    from safetensors.torch import load_file

    a = load_file(str(tmp_path / "full" / "adapter" / "adapter_model.safetensors"))
    b = load_file(str(tmp_path / "split" / "adapter" / "adapter_model.safetensors"))
    assert a.keys() == b.keys()
    for k in a:
        assert torch.allclose(a[k], b[k], atol=1e-5), k


def test_qlora_without_bitsandbytes_degrades_or_fails(tiny, tmp_path, monkeypatch):
    monkeypatch.setattr(T, "bitsandbytes_status", lambda torch=None: (False, "bitsandbytes is not usable here"))
    result, _ = _run(_args(tiny, tmp_path / "q", max_steps=1, eval_fraction=0.0), tmp_path, qlora=True)
    assert result["load_in_4bit"] is False and "16-bit LoRA instead" in result["notes"][0]
    with pytest.raises(T.TrainError, match="bitsandbytes"):
        T.run(_args(tiny, tmp_path / "q2", qlora_fallback=False), Emitter(open(tmp_path / "e2", "w")), qlora=True)


def test_missing_gpu_and_empty_data_fail_clearly(tiny, tmp_path):
    if not torch.cuda.is_available():
        with pytest.raises(T.TrainError, match="device=cpu"):
            T.run(_args(tiny, tmp_path / "x", device="auto"), Emitter(open(tmp_path / "e", "w")))
    bad = tmp_path / "bad.jsonl"
    bad.write_text('{"nothing": 1}\n')
    with pytest.raises(T.TrainError, match="usable"):
        T.run(_args(tiny, tmp_path / "y", data=str(bad)), Emitter(open(tmp_path / "e", "w")))


def test_merge_lora_makes_a_loadable_model(tiny, tmp_path):
    from mm_forge import export

    _run(_args(tiny, tmp_path / "m", max_steps=1, eval_fraction=0.0), tmp_path)
    em = Emitter(open(tmp_path / "e", "w"))
    r = export.run_merge({"base_model": str(tiny), "adapter": str(tmp_path / "m" / "adapter"),
                          "out_dir": str(tmp_path / "merged"), "dtype": "float32"}, em)
    assert r["files"] and (tmp_path / "merged" / "config.json").is_file()
    m = transformers.AutoModelForCausalLM.from_pretrained(str(tmp_path / "merged"))
    assert not any("lora" in n for n, _ in m.named_parameters())
