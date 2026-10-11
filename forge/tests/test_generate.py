"""The generation server on a tiny random Llama (no download, no tokenizer: token ids)."""

import io
import json
import math

import pytest

torch = pytest.importorskip("torch")
transformers = pytest.importorskip("transformers")

from mm_forge import generate  # noqa: E402
from mm_forge.protocol import Emitter, read_events  # noqa: E402


@pytest.fixture(scope="module")
def model_dir(tmp_path_factory):
    d = tmp_path_factory.mktemp("tiny")
    torch.manual_seed(0)
    cfg = transformers.LlamaConfig(vocab_size=64, hidden_size=32, intermediate_size=64, num_hidden_layers=2,
                                   num_attention_heads=4, num_key_value_heads=2, eos_token_id=63)
    transformers.LlamaForCausalLM(cfg).save_pretrained(d)
    return d


def test_pick_device_rules():
    assert generate.pick_device("cpu", 10, True, 10**12) == "cpu"
    assert generate.pick_device("gpu", 10**15, True, 1) == "cuda:0"
    with pytest.raises(RuntimeError):
        generate.pick_device("gpu", 1, False)
    assert generate.pick_device("auto", 100, True, 1000) == "cuda:0"
    assert generate.pick_device("auto", 100, True, 110) == "cpu", "no room for the cache: stay on the CPU"
    assert generate.pick_device("auto", 1, False, 0) == "cpu"
    with pytest.raises(ValueError):
        generate.pick_device("tpu", 1, False)


def test_greedy_tokens_and_logprobs_match_transformers(model_dir):
    m = generate.Model({"model_path": str(model_dir), "device": "cpu"})
    assert m.info["device"] == "cpu" and m.info["tokenizer"] is False and m.info["parameters"] > 0
    ids = [1, 5, 9, 2]
    r = m.generate({"prompt_ids": ids, "max_tokens": 8, "top_logprobs": 3})
    ref = m.model.generate(torch.tensor([ids]), max_new_tokens=8, do_sample=False, eos_token_id=63, pad_token_id=0)
    got = [t["id"] for t in r["tokens"]]
    assert got == ref[0, len(ids):].tolist()[: len(got)]
    for t in r["tokens"]:
        assert t["logprob"] <= 0 and math.isfinite(t["logprob"])
        assert len(t["top"]) == 3 and t["top"][0]["logprob"] >= t["top"][1]["logprob"]
        assert t["top"][0]["id"] == t["id"], "greedy picks the most likely token"
    assert r["prompt_tokens"] == 4 and r["completion_tokens"] == len(got) and r["finish"] in ("length", "stop")


def test_sampling_is_seeded_and_stop_ids_end_the_text(model_dir):
    m = generate.Model({"model_path": str(model_dir), "device": "cpu"})
    req = {"prompt_ids": [3, 4], "max_tokens": 6, "temperature": 1.0, "seed": 11, "top_p": 0.9}
    a, b = m.generate(req), m.generate(req)
    assert [t["id"] for t in a["tokens"]] == [t["id"] for t in b["tokens"]]
    first = m.generate({"prompt_ids": [3, 4], "max_tokens": 6})["tokens"][0]["id"]
    s = m.generate({"prompt_ids": [3, 4], "max_tokens": 6, "stop_ids": [first]})
    assert s["finish"] == "stop" and len(s["tokens"]) == 1


def test_bad_requests_are_errors(model_dir):
    m = generate.Model({"model_path": str(model_dir), "device": "cpu"})
    for bad in ({}, {"prompt_ids": []}, {"prompt": "text needs a tokenizer"}):
        with pytest.raises(ValueError):
            m.generate(bad)


def test_the_server_answers_each_request_by_id_and_survives_a_bad_one(model_dir):
    out = io.StringIO()
    reqs = "\n".join([
        json.dumps({"id": "a", "prompt_ids": [1, 2], "max_tokens": 3}),
        "not json",
        json.dumps({"id": "b", "prompt_ids": []}),
        json.dumps({"id": "c", "prompt_ids": [1, 2], "max_tokens": 2, "top_logprobs": 2}),
        json.dumps({"id": "z", "op": "shutdown"}),
        json.dumps({"id": "never", "prompt_ids": [1]}),
    ])
    final = generate.serve({"model_path": str(model_dir), "device": "cpu"}, Emitter(out), io.StringIO(reqs))
    assert final == {"stopped": True}
    res = [e["data"] for e in read_events(out.getvalue().splitlines()) if e["type"] == "result"]
    by = {r.get("id"): r for r in res}
    assert by["init"]["device"] == "cpu"
    assert len(by["a"]["tokens"]) == 3 and len(by["c"]["tokens"]) == 2
    assert "error" in by["b"] and "error" in by[None]
    assert "never" not in by


def test_the_same_model_runs_on_the_gpu_when_there_is_one(model_dir):
    if not torch.cuda.is_available():
        pytest.skip("no GPU")
    cpu = generate.Model({"model_path": str(model_dir), "device": "cpu", "dtype": "float32"})
    gpu = generate.Model({"model_path": str(model_dir), "device": "gpu", "dtype": "float32"})
    assert gpu.info["device"].startswith("cuda")
    a = cpu.generate({"prompt_ids": [1, 5, 9], "max_tokens": 6})
    b = gpu.generate({"prompt_ids": [1, 5, 9], "max_tokens": 6})
    assert [t["id"] for t in a["tokens"]] == [t["id"] for t in b["tokens"]]
    assert abs(a["tokens"][0]["logprob"] - b["tokens"][0]["logprob"]) < 1e-3
