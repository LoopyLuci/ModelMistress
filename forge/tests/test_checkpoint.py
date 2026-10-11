import json

import pytest

torch = pytest.importorskip("torch")
from safetensors import safe_open  # noqa: E402

from mm_forge.checkpoint import (Spec, load_checkpoint, plan_shards, save_checkpoint, verify_checkpoint,  # noqa: E402
                                 write_safetensors, write_sharded)


def test_checkpoint_round_trip_and_tamper_detection(tmp_path):
    t = {"a": torch.randn(3, 4), "b": torch.arange(5, dtype=torch.int64)}
    m = save_checkpoint(tmp_path / "ck", t, {"step": 12})
    assert m["meta"] == {"step": 12} and verify_checkpoint(tmp_path / "ck")
    got, meta = load_checkpoint(tmp_path / "ck")
    assert meta["step"] == 12 and torch.equal(got["a"], t["a"]) and torch.equal(got["b"], t["b"])
    with open(tmp_path / "ck" / "weights.safetensors", "r+b") as f:
        f.seek(-1, 2)
        f.write(b"\x01")
    assert not verify_checkpoint(tmp_path / "ck")
    with pytest.raises(ValueError):
        load_checkpoint(tmp_path / "ck")


def test_plan_shards_is_greedy_and_gives_big_tensors_their_own_shard():
    specs = [Spec(f"t{i}", "F32", (n,)) for i, n in enumerate([10, 10, 100, 5])]
    shards = plan_shards(specs, 100)  # bytes: 40 + 40, 400, 20
    assert [[s.name for s in sh] for sh in shards] == [["t0", "t1"], ["t2"], ["t3"]]


def test_zero_tensors_read_back_as_zeros_and_made_ones_as_made(tmp_path):
    a = torch.randn(4, 6).to(torch.bfloat16)
    specs = [Spec("first", "BF16", (4, 6), lambda: a), Spec("hole", "BF16", (8, 16), None),
             Spec("last", "F32", (3,), lambda: torch.tensor([1.0, 2.0, 3.0])), Spec("tail_hole", "F16", (100,), None)]
    size = write_safetensors(tmp_path / "x.safetensors", specs, {"k": "v"})
    assert size > 0
    with safe_open(str(tmp_path / "x.safetensors"), framework="pt") as f:
        assert f.metadata() == {"k": "v"}
        assert torch.equal(f.get_tensor("first"), a)
        assert not f.get_tensor("hole").any() and f.get_tensor("hole").shape == (8, 16)
        assert f.get_tensor("last").tolist() == [1.0, 2.0, 3.0]
        assert f.get_tensor("tail_hole").dtype == torch.float16 and not f.get_tensor("tail_hole").any()


def test_wrong_size_from_a_maker_is_an_error(tmp_path):
    with pytest.raises(ValueError):
        write_safetensors(tmp_path / "bad.safetensors", [Spec("x", "F32", (4,), lambda: torch.zeros(3))])


def test_write_sharded_makes_an_index(tmp_path):
    specs = [Spec(f"t{i}", "F32", (64,), lambda i=i: torch.full((64,), float(i))) for i in range(5)]
    info = write_sharded(tmp_path, specs, max_shard_bytes=600)
    assert len(info["files"]) == 3 and info["tensors"] == 5
    idx = json.loads((tmp_path / "model.safetensors.index.json").read_text())
    assert idx["metadata"]["total_size"] == 5 * 256 and set(idx["weight_map"]) == {f"t{i}" for i in range(5)}
    with safe_open(str(tmp_path / idx["weight_map"]["t4"]), framework="pt") as f:
        assert f.get_tensor("t4").tolist() == [4.0] * 64
