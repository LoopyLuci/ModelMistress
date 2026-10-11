"""Doubling a weight must double its Q8_0 block exactly, including where the float16 scale is subnormal."""

import io
import sys
from pathlib import Path

import pytest

np = pytest.importorskip("numpy")
torch = pytest.importorskip("torch")
from safetensors.torch import save_file  # noqa: E402

from mm_forge import dusty  # noqa: E402
from mm_forge.protocol import Emitter  # noqa: E402

CONVERTER = Path(dusty.__file__).resolve().parents[2] / "engines" / "llama.cpp" / "convert_hf_to_gguf.py"
pytestmark = pytest.mark.skipif(not CONVERTER.is_file(), reason="llama.cpp source not present")


def test_the_naive_doubling_really_differs_for_small_blocks():
    # the reason for the fix: f16(2 * d) is not 2 * f16(d) when d is subnormal
    bad = [x for x in np.linspace(1e-6, 5e-5, 400, dtype=np.float32) if np.float16(2 * x) != 2 * np.float16(x)]
    assert bad, "subnormal scales do not double exactly"


def test_fix_makes_doubled_blocks_exactly_twice_the_original_scales(tmp_path):
    sys.path.insert(0, str(CONVERTER.parent / "gguf-py"))
    import gguf

    rows, cols = 300, 64
    g = torch.Generator().manual_seed(1)
    w = (torch.randn(rows, cols, generator=g) * 0.02).to(torch.bfloat16)
    # blocks whose scale is subnormal in float16 (amax/127 < 6.1e-5), at many different magnitudes
    w[:200, :32] = (torch.randn(200, 32, generator=g) * torch.linspace(1e-4, 5e-3, 200)[:, None]).to(torch.bfloat16)
    src = tmp_path / "src"
    src.mkdir()
    name = "model.language_model.layers.0.mlp.down_proj.weight"
    save_file({name: w}, str(src / "model.safetensors"))

    def q8(t):
        return gguf.quants.quantize(t.float().numpy(), gguf.GGMLQuantizationType.Q8_0)

    orig, naive = q8(w), q8(w * 2)
    out = tmp_path / "m.gguf"
    wr = gguf.GGUFWriter(str(out), "llama")
    wr.add_tensor("blk.0.ffn_down_shexp.weight", naive, raw_dtype=gguf.GGMLQuantizationType.Q8_0)
    wr.write_header_to_file()
    wr.write_kv_data_to_file()
    wr.write_tensors_to_file()
    wr.close()

    def scales(a):
        return np.ascontiguousarray(a).reshape(-1, 34)[:, :2].copy().view(np.float16).ravel()

    assert (scales(naive) != scales(orig) * 2).any(), "the test needs a block that the naive way gets wrong"
    info = dusty.fix_q8_doubled_scales(out, src, CONVERTER, Emitter(io.StringIO()))
    assert info["tensors"] == 1 and info["blocks_rescaled"] >= 1
    t = next(t for t in gguf.GGUFReader(str(out)).tensors if t.name == "blk.0.ffn_down_shexp.weight")
    fixed = np.ascontiguousarray(t.data).reshape(-1, 34)
    assert (scales(fixed) == scales(orig) * 2).all()
    assert (fixed[:, 2:] == np.ascontiguousarray(orig).reshape(-1, 34)[:, 2:]).all(), "the int8 values are untouched"
