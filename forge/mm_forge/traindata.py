"""Training data for the fine-tuning steps: reading alpaca / ShareGPT / chat-JSONL files, turning every row into chat
messages, a split into train and eval sets, and tokenizing with the loss on the assistant's turns only.

Formats (detected per row, so a file may mix them):

* chat:     {"messages": [{"role": "user", "content": "..."}, {"role": "assistant", "content": "..."}]}
* ShareGPT: {"conversations": [{"from": "human", "value": "..."}, {"from": "gpt", "value": "..."}]}
* alpaca:   {"instruction": "...", "input": "...", "output": "..."} (an optional "system")
* also prompt/completion, question/answer and plain {"text": "..."} (the whole text is trained on).

Files: `.jsonl` (one object per line), `.json` (a list, or {"data": [...]}), `.parquet` when `datasets` is installed.
The helpers here need no torch; only `encode` needs a tokenizer.
"""

from __future__ import annotations

import json
import random
from pathlib import Path
from typing import Any

ROLES = {"human": "user", "user": "user", "gpt": "assistant", "assistant": "assistant", "model": "assistant",
         "bot": "assistant", "system": "system", "tool": "tool"}

# Used when the base model brings no chat template (a base model): the template is saved with the result.
CHATML = ("{% for m in messages %}<|im_start|>{{ m['role'] }}\n{{ m['content'] }}<|im_end|>\n{% endfor %}"
          "{% if add_generation_prompt %}<|im_start|>assistant\n{% endif %}")


def detect_format(row: dict) -> str | None:
    if isinstance(row.get("messages"), list):
        return "chat"
    if isinstance(row.get("conversations"), list):
        return "sharegpt"
    if "instruction" in row and ("output" in row or "response" in row):
        return "alpaca"
    for q, a in (("prompt", "completion"), ("question", "answer")):
        if q in row and a in row:
            return "prompt_completion"
    if isinstance(row.get("text"), str):
        return "text"
    return None


def to_messages(row: dict) -> list[dict] | None:
    """One row -> [{"role", "content"}...], or None if the row has no known shape. A plain-text row becomes a single
    "text" message (see `encode`: trained on in full)."""
    fmt = detect_format(row)
    if fmt == "chat":
        return [{"role": ROLES.get(str(m.get("role", "")).lower(), m.get("role")), "content": str(m.get("content") or "")}
                for m in row["messages"]]
    if fmt == "sharegpt":
        return [{"role": ROLES.get(str(m.get("from") or m.get("role") or "").lower(), "user"),
                 "content": str(m.get("value") or m.get("content") or "")} for m in row["conversations"]]
    if fmt == "alpaca":
        user = str(row["instruction"]) + (("\n\n" + str(row["input"])) if row.get("input") else "")
        msgs = [{"role": "system", "content": str(row["system"])}] if row.get("system") else []
        out = row.get("output") if "output" in row else row.get("response")
        return msgs + [{"role": "user", "content": user}, {"role": "assistant", "content": str(out)}]
    if fmt == "prompt_completion":
        q, a = ("prompt", "completion") if "prompt" in row else ("question", "answer")
        return [{"role": "user", "content": str(row[q])}, {"role": "assistant", "content": str(row[a])}]
    if fmt == "text":
        return [{"role": "text", "content": row["text"]}]
    return None


def load_rows(paths: list[str] | str) -> list[dict]:
    rows: list[dict] = []
    for p in [paths] if isinstance(paths, str) else paths:
        p = Path(p)
        suffix = p.suffix.lower()
        if suffix == ".json":
            data = json.loads(p.read_text(encoding="utf-8"))
            rows += data if isinstance(data, list) else data.get("data", [])
        elif suffix == ".parquet":
            import datasets  # noqa: PLC0415

            rows += list(datasets.load_dataset("parquet", data_files=str(p), split="train"))
        else:
            for n, line in enumerate(p.read_text(encoding="utf-8").splitlines(), 1):
                if line.strip():
                    try:
                        rows.append(json.loads(line))
                    except ValueError as e:
                        raise ValueError(f"{p}:{n}: not JSON ({e})") from e
    return rows


def usable(msgs: list[dict] | None) -> bool:
    return bool(msgs) and any(m["role"] in ("assistant", "text") and m["content"].strip() for m in msgs)


def load_examples(paths: list[str] | str, max_examples: int | None = None) -> tuple[list[list[dict]], dict]:
    """(examples, stats): the usable rows as message lists, and counts per format / skipped rows."""
    rows = load_rows(paths)
    out: list[list[dict]] = []
    formats: dict[str, int] = {}
    skipped = 0
    for r in rows:
        if not isinstance(r, dict):
            skipped += 1
            continue
        msgs = to_messages(r)
        if not usable(msgs):
            skipped += 1
            continue
        fmt = detect_format(r)
        formats[fmt] = formats.get(fmt, 0) + 1
        out.append(msgs)
        if max_examples and len(out) >= max_examples:
            break
    return out, {"rows": len(rows), "usable": len(out), "skipped": skipped, "formats": formats}


def split(examples: list, eval_fraction: float = 0.0, seed: int = 3407, max_eval: int = 200) -> tuple[list, list]:
    """Shuffles (seeded) and splits into (train, eval). An eval set is at most `max_eval` rows and leaves at least
    one training row."""
    ex = list(examples)
    random.Random(seed).shuffle(ex)
    n_eval = min(int(len(ex) * max(0.0, eval_fraction)), max_eval, max(0, len(ex) - 1))
    return ex[n_eval:], ex[:n_eval]


def encode(tok: Any, msgs: list[dict], max_len: int) -> tuple[list[int], list[int]]:
    """(input_ids, labels): -100 everywhere except the assistant's turns (and the end-of-turn token); a "text" row
    is trained on in full. Works in text (character spans of the rendered chat), which survives chat templates that
    rewrite history."""
    if msgs and msgs[0]["role"] == "text":
        ids = tok(msgs[0]["content"] + (tok.eos_token or ""), add_special_tokens=True)["input_ids"][:max_len]
        return ids, list(ids)
    full = tok.apply_chat_template(msgs, tokenize=False)
    spans, pos = [], 0
    for i, m in enumerate(msgs):
        if m["role"] != "assistant":
            continue
        pre = tok.apply_chat_template(msgs[:i], tokenize=False, add_generation_prompt=True) if i else ""
        upto = tok.apply_chat_template(msgs[: i + 1], tokenize=False)
        if full.startswith(upto) and upto.startswith(pre):
            spans.append((len(pre), len(upto)))
        else:
            at = full.find(m["content"], pos)
            if at < 0:
                continue
            spans.append((at, at + len(m["content"])))
        pos = spans[-1][1]
    enc = tok(full, add_special_tokens=False, return_offsets_mapping=True)
    ids, labels = list(enc["input_ids"]), []
    for t, (a, b) in zip(ids, enc["offset_mapping"]):
        labels.append(t if b > a and any(s <= a and b <= e for s, e in spans) else -100)
    return ids[:max_len], labels[:max_len]


def encode_all(tok: Any, examples: list[list[dict]], max_len: int) -> list[tuple[list[int], list[int]]]:
    items = []
    for m in examples:
        ids, lab = encode(tok, m, max_len)
        if any(x != -100 for x in lab):  # an example cut before its first answer token teaches nothing
            items.append((ids, lab))
    return items


def batch_order(n: int, batch_size: int, seed: int, lengths: list[int] | None = None) -> list[list[int]]:
    """Index batches for one epoch: shuffled with `seed`, grouped by length inside windows of 50 batches (less
    padding), batches in random order. Deterministic, so a resumed run can skip the batches it already did."""
    rnd = random.Random(seed)
    idx = list(range(n))
    rnd.shuffle(idx)
    w = batch_size * 50
    groups = [sorted(idx[i:i + w], key=(lambda j: lengths[j]) if lengths else (lambda j: 0)) for i in range(0, n, w)]
    out = [g[i:i + batch_size] for g in groups for i in range(0, len(g), batch_size)]
    rnd.shuffle(out)
    return out


def collate(items: list[tuple[list[int], list[int]]], batch: list[int], pad: int):
    import torch

    n = max(len(items[j][0]) for j in batch)
    ids = torch.full((len(batch), n), pad, dtype=torch.long)
    lab = torch.full((len(batch), n), -100, dtype=torch.long)
    att = torch.zeros((len(batch), n), dtype=torch.long)
    for r, j in enumerate(batch):
        x, y = items[j]
        ids[r, : len(x)] = torch.tensor(x)
        lab[r, : len(y)] = torch.tensor(y)
        att[r, : len(x)] = 1
    return ids, lab, att
