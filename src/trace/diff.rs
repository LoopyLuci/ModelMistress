//! Comparing two traces: the answers word by word, the settings that differ, the adapters, and where the two
//! generations first parted ways token by token.

use serde_json::{json, Value};

use super::store::TokenRec;

fn words(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut prev: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let ws = c.is_whitespace();
        if prev.is_some_and(|p| p != ws) {
            out.push(&s[start..i]);
            start = i;
        }
        prev = Some(ws);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn push(ops: &mut Vec<(char, String)>, op: char, text: &str) {
    match ops.last_mut() {
        Some((o, t)) if *o == op => t.push_str(text),
        _ => ops.push((op, text.to_string())),
    }
}

/// A word diff: ops are ("eq" | "del" | "ins", text), `del` is in `a` only and `ins` in `b` only.
pub fn text_diff(a: &str, b: &str) -> Value {
    let (x, y) = (words(a), words(b));
    let mut pre = 0;
    while pre < x.len() && pre < y.len() && x[pre] == y[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < x.len() - pre && suf < y.len() - pre && x[x.len() - 1 - suf] == y[y.len() - 1 - suf] {
        suf += 1;
    }
    let (mx, my) = (&x[pre..x.len() - suf], &y[pre..y.len() - suf]);
    let mut ops: Vec<(char, String)> = vec![];
    for w in &x[..pre] {
        push(&mut ops, '=', w);
    }
    if mx.len() * my.len() <= 4_000_000 {
        // longest common subsequence of the middle
        let (n, m) = (mx.len(), my.len());
        let mut t = vec![0u32; (n + 1) * (m + 1)];
        let at = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                t[at(i, j)] = if mx[i] == my[j] {
                    t[at(i + 1, j + 1)] + 1
                } else {
                    t[at(i + 1, j)].max(t[at(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if mx[i] == my[j] {
                push(&mut ops, '=', mx[i]);
                i += 1;
                j += 1;
            } else if t[at(i + 1, j)] >= t[at(i, j + 1)] {
                push(&mut ops, '-', mx[i]);
                i += 1;
            } else {
                push(&mut ops, '+', my[j]);
                j += 1;
            }
        }
        for w in &mx[i..] {
            push(&mut ops, '-', w);
        }
        for w in &my[j..] {
            push(&mut ops, '+', w);
        }
    } else {
        for w in mx {
            push(&mut ops, '-', w);
        }
        for w in my {
            push(&mut ops, '+', w);
        }
    }
    for w in &x[x.len() - suf..] {
        push(&mut ops, '=', w);
    }
    let same: usize = ops.iter().filter(|(o, _)| *o == '=').map(|(_, t)| words(t).len()).sum();
    let denom = x.len().max(y.len()).max(1);
    json!({"equal": a == b, "similarity": same as f64 / denom as f64,
           "ops": ops.iter().map(|(o, t)| json!({"op": match o { '=' => "eq", '-' => "del", _ => "ins" }, "text": t}))
                      .collect::<Vec<_>>()})
}

fn stats(t: &[TokenRec]) -> Value {
    let n = t.len().max(1) as f64;
    let mean_lp = t.iter().map(|x| x.logprob as f64).sum::<f64>() / n;
    let mean_h = t.iter().map(|x| x.entropy as f64).sum::<f64>() / n;
    let low = t.iter().filter(|x| x.logprob.exp() < 0.5).count();
    json!({"count": t.len(), "mean_logprob": mean_lp, "mean_entropy": mean_h, "uncertain": low})
}

fn summary(t: &Value) -> Value {
    json!({"id": t["id"], "model": t["model"], "status": t["status"], "token_count": t["token_count"],
           "duration_ms": t["duration_ms"], "parent_id": t["parent_id"],
           "tokens_per_s": t["timings"]["predicted_per_second"], "wall_ms": t["timings"]["wall_ms"]})
}

fn changed_keys(a: &Value, b: &Value) -> Vec<Value> {
    let (ao, bo) = (
        a.as_object().cloned().unwrap_or_default(),
        b.as_object().cloned().unwrap_or_default(),
    );
    let mut keys: Vec<&String> = ao.keys().chain(bo.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| ao.get(*k) != bo.get(*k))
        .map(|k| json!({"key": k, "a": ao.get(k).cloned().unwrap_or(Value::Null), "b": bo.get(k).cloned().unwrap_or(Value::Null)}))
        .collect()
}

fn adapter_scales(t: &Value) -> Value {
    let list = t["adapters"].as_array().cloned().unwrap_or_default();
    json!(list
        .iter()
        .map(|a| json!({"index": a["index"], "name": a["name"], "scale": a["effective_scale"]}))
        .collect::<Vec<_>>())
}

/// `a` and `b` are `Store::get` answers; `ta` and `tb` their tokens.
pub fn diff(a: &Value, ta: &[TokenRec], b: &Value, tb: &[TokenRec]) -> Value {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    let first = ta.iter().zip(tb).position(|(x, y)| x.text != y.text);
    let first = first.or_else(|| (ta.len() != tb.len()).then_some(ta.len().min(tb.len())));
    let divergence = first.map(|i| {
        let side = |t: &[TokenRec]| {
            t.get(i).map(|x| {
                json!({"text": x.text, "p": x.logprob.exp(),
                       "alts": x.alts.iter().map(|a| json!({"text": a.text, "p": a.logprob.exp()})).collect::<Vec<_>>()})
            })
        };
        let context: String = ta[..i.min(ta.len())].iter().rev().take(12).rev().map(|x| x.text.as_str()).collect();
        json!({"index": i, "context": context, "a": side(ta), "b": side(tb)})
    });
    let adapters_a = adapter_scales(a);
    let adapters_b = adapter_scales(b);
    json!({
        "a": summary(a), "b": summary(b),
        "same_model": a["model"] == b["model"],
        "same_prompt": a["prompt"] == b["prompt"] && a["rendered_prompt"] == b["rendered_prompt"],
        "params": changed_keys(&a["params"], &b["params"]),
        "adapters": {"a": adapters_a, "b": adapters_b, "changed": adapters_a != adapters_b},
        "answer": text_diff(&s(a, "answer"), &s(b, "answer")),
        "reasoning": text_diff(&s(a, "reasoning"), &s(b, "reasoning")),
        "tokens": {"a": stats(ta), "b": stats(tb), "first_divergence": divergence},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_diff_keeps_what_is_common() {
        let d = text_diff("the cat sat down", "the dog sat down");
        let ops = d["ops"].as_array().unwrap();
        let text = |o: &Value| format!("{}:{}", o["op"].as_str().unwrap(), o["text"].as_str().unwrap());
        assert_eq!(
            ops.iter().map(text).collect::<Vec<_>>(),
            ["eq:the ", "del:cat", "ins:dog", "eq: sat down"]
        );
        assert_eq!(d["equal"], false);
        assert!(text_diff("same", "same")["equal"].as_bool().unwrap());
        assert_eq!(text_diff("", "")["ops"].as_array().unwrap().len(), 0);
    }
}
