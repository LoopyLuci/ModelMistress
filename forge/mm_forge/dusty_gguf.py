"""llama.cpp's convert_hf_to_gguf.py with the routed experts of a "virtual" Dusty checkpoint made on the fly.

    python -m mm_forge.dusty_gguf --converter <path to convert_hf_to_gguf.py> <the converter's own arguments>

A virtual checkpoint (see `dusty`) has no routed-expert tensors on disk; this driver adds them to the converter's
tensor index as lazy tensors, so the converter handles them like any other (splitting gate and up, naming, type
conversion, writing) and only one layer's experts exist in memory at a time. Nothing else of the converter is
changed.
"""

from __future__ import annotations

import sys
from pathlib import Path


def main(argv: list[str]) -> int:
    if len(argv) < 2 or argv[0] != "--converter":
        print(__doc__)
        return 2
    script = Path(argv[1]).resolve()
    sys.path.insert(0, str(script.parent))
    sys.argv = [str(script)] + argv[2:]

    import torch
    import convert_hf_to_gguf as conv  # noqa: E402  (adds llama.cpp's gguf-py to the path)
    from conversion.base import LazyTorchTensor
    from conversion.qwen import Qwen3_5MoeTextModel

    from .dusty import VIRTUAL_FILE, virtual_specs

    original = Qwen3_5MoeTextModel.index_tensors

    def with_virtual_experts(self, remote_hf_model_id=None):
        tensors = original(self, remote_hf_model_id)
        if (Path(self.dir_model) / VIRTUAL_FILE).is_file():
            dtypes = {"BF16": torch.bfloat16, "F16": torch.float16, "F32": torch.float32}
            for spec in virtual_specs(Path(self.dir_model)):
                dtype = dtypes[spec.dtype]

                def make(s=spec, d=dtype):
                    return s.make() if s.make is not None else torch.zeros(s.shape, dtype=d)

                if self.lazy:
                    def gen(s=spec, d=dtype, m=make):
                        meta = LazyTorchTensor.meta_with_dtype_and_shape(d, tuple(s.shape))
                        return LazyTorchTensor(meta=meta, args=(), func=lambda: m())
                    tensors[spec.name] = gen
                else:
                    tensors[spec.name] = make
        return tensors

    Qwen3_5MoeTextModel.index_tensors = with_virtual_experts
    conv.main()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
