from mm_forge import equiv


def comp(ids, lps, text="x", tops=None):
    return {"finish": "length", "text": text, "ids": ids, "logprobs": lps,
            "tops": tops or [[(i, lp)] for i, lp in zip(ids, lps)]}


def test_identical_completions_compare_equal():
    a = comp([1, 2, 3], [-0.1, -0.2, -0.3])
    c = equiv.compare(a, comp([1, 2, 3], [-0.1, -0.2, -0.3]), 1e-3)
    assert c["ids_equal"] and c["text_equal"] and c["within_tolerance"]
    assert c["first_diff"] is None and c["max_logprob_diff"] == 0.0


def test_a_different_token_is_found_where_it_happens():
    c = equiv.compare(comp([1, 2, 3], [-0.1] * 3), comp([1, 9, 3], [-0.1] * 3), 1e-3)
    assert not c["ids_equal"] and c["first_diff"] == 1


def test_a_shorter_answer_differs_at_its_end():
    c = equiv.compare(comp([1, 2, 3], [-0.1] * 3), comp([1, 2], [-0.1] * 2), 1e-3)
    assert not c["ids_equal"] and c["first_diff"] == 2 and c["tokens"] == [3, 2]


def test_logprob_differences_are_measured_against_the_tolerance():
    a = comp([1, 2], [-0.1, -0.2])
    near = equiv.compare(a, comp([1, 2], [-0.1004, -0.2]), 1e-3)
    far = equiv.compare(a, comp([1, 2], [-0.1, -0.25]), 1e-3)
    assert near["within_tolerance"] and not far["within_tolerance"]
    assert abs(far["max_logprob_diff"] - 0.05) < 1e-9


def test_there_are_ten_distinct_builtin_prompts():
    assert len(equiv.PROMPTS) >= 10 and len(set(equiv.PROMPTS)) == len(equiv.PROMPTS)
