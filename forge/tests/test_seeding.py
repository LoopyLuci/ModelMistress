import pytest

from mm_forge.seeding import derive_seed, seed_everything


def test_derive_seed_is_stable_and_separates_keys():
    assert derive_seed(1, "router", 0) == derive_seed(1, "router", 0)
    assert derive_seed(1, "router", 0) != derive_seed(1, "router", 1)
    assert derive_seed(1, "router", 0) != derive_seed(2, "router", 0)
    assert 0 <= derive_seed(5, "x") < 2**63


def test_seed_everything_makes_torch_reproducible():
    torch = pytest.importorskip("torch")
    seed_everything(42, deterministic=False)
    a = torch.randn(5)
    seed_everything(42, deterministic=False)
    b = torch.randn(5)
    assert torch.equal(a, b)
    from mm_forge.seeding import generator

    assert torch.equal(torch.randn(3, generator=generator(7)), torch.randn(3, generator=generator(7)))
