//! Executable checks run during verification (`km-factory.md` section 2, step 4): where a record can be checked by a
//! program rather than by a model, it is. The hook is a trait with a no-op default; the built-in check validates the
//! arguments of a tool-call record against the tool's JSON schema. A check never runs anything the record contains.

use serde_json::Value;

/// A tool the task knows (from a hub operation or an MCP tool definition given inline).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub provider: String,
    pub description: String,
    pub input_schema: Value,
    pub side_effects: String,
}

impl ToolDef {
    pub fn to_json(&self) -> Value {
        serde_json::json!({"name": self.name, "provider": self.provider, "description": self.description,
                           "input_schema": self.input_schema, "side_effects": self.side_effects})
    }

    pub fn from_json(v: &Value) -> Option<ToolDef> {
        Some(ToolDef {
            name: v["name"].as_str()?.to_string(),
            provider: v["provider"].as_str().unwrap_or("hub").to_string(),
            description: v["description"].as_str().unwrap_or("").to_string(),
            input_schema: v.get("input_schema").cloned().unwrap_or(Value::Null),
            side_effects: v["side_effects"].as_str().unwrap_or("none").to_string(),
        })
    }
}

pub struct CheckInput<'a> {
    /// The record (km-spec.md section 8, plus `tool_calls` for tool-use records).
    pub record: &'a Value,
    pub tools: &'a [ToolDef],
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckOutcome {
    /// Did the check apply to this record?
    pub ran: bool,
    pub ok: bool,
    pub note: String,
}

impl CheckOutcome {
    pub fn skipped() -> Self {
        Self {
            ran: false,
            ok: true,
            note: String::new(),
        }
    }
}

pub trait Check: Send + Sync {
    fn name(&self) -> &str;
    /// The default does nothing, so a check only has to say what it does.
    fn run(&self, _input: &CheckInput) -> CheckOutcome {
        CheckOutcome::skipped()
    }
}

/// Does nothing; the default when no other check applies.
pub struct NoopCheck;

impl Check for NoopCheck {
    fn name(&self) -> &str {
        "noop"
    }
}

/// A tool-call record's calls must name known tools and their arguments must satisfy the tool's input schema.
pub struct ToolSchemaCheck;

impl Check for ToolSchemaCheck {
    fn name(&self) -> &str {
        "tool-schema"
    }

    fn run(&self, input: &CheckInput) -> CheckOutcome {
        let Some(calls) = input.record["tool_calls"].as_array().filter(|c| !c.is_empty()) else {
            return CheckOutcome::skipped();
        };
        let mut problems = vec![];
        for (i, c) in calls.iter().enumerate() {
            let name = c["name"].as_str().unwrap_or("");
            let Some(tool) = input.tools.iter().find(|t| t.name == name) else {
                problems.push(format!("call {i}: unknown tool {name:?}"));
                continue;
            };
            if tool.input_schema.is_null() {
                continue;
            }
            let args = c.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));
            for e in validate(&tool.input_schema, &args) {
                problems.push(format!("call {i} ({name}): {e}"));
            }
        }
        CheckOutcome {
            ran: true,
            ok: problems.is_empty(),
            note: problems.join("; "),
        }
    }
}

/// The checks every task runs.
pub fn default_checks() -> Vec<std::sync::Arc<dyn Check>> {
    vec![std::sync::Arc::new(NoopCheck), std::sync::Arc::new(ToolSchemaCheck)]
}

// ---- a small JSON Schema validator -----------------------------------------------------------------------------
// Covers what tool schemas use: type, enum, const, required, properties, additionalProperties (false), items,
// minimum/maximum, minLength/maxLength, minItems/maxItems, oneOf/anyOf. Unknown keywords are ignored.

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0 && v.is_number()),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    }
}

pub fn validate(schema: &Value, v: &Value) -> Vec<String> {
    let mut out = vec![];
    check(schema, v, "$", &mut out);
    out
}

fn check(schema: &Value, v: &Value, path: &str, out: &mut Vec<String>) {
    let Some(s) = schema.as_object() else { return };
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(t) => type_ok(t, v),
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)),
            _ => true,
        };
        if !ok {
            out.push(format!("{path}: expected {t}, got {}", kind_of(v)));
            return;
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        if !e.contains(v) {
            out.push(format!("{path}: {v} is not one of the allowed values"));
        }
    }
    if let Some(c) = s.get("const") {
        if c != v {
            out.push(format!("{path}: must be {c}"));
        }
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(alts) = s.get(key).and_then(Value::as_array) {
            let matching = alts.iter().filter(|a| validate(a, v).is_empty()).count();
            if matching == 0 || (key == "oneOf" && matching > 1) {
                out.push(format!("{path}: does not match {key}"));
            }
        }
    }
    if let Some(n) = v.as_f64() {
        if let Some(m) = s.get("minimum").and_then(Value::as_f64) {
            if n < m {
                out.push(format!("{path}: {n} is below the minimum {m}"));
            }
        }
        if let Some(m) = s.get("maximum").and_then(Value::as_f64) {
            if n > m {
                out.push(format!("{path}: {n} is above the maximum {m}"));
            }
        }
    }
    if let Some(t) = v.as_str() {
        let len = t.chars().count() as u64;
        if s.get("minLength").and_then(Value::as_u64).is_some_and(|m| len < m) {
            out.push(format!("{path}: shorter than minLength"));
        }
        if s.get("maxLength").and_then(Value::as_u64).is_some_and(|m| len > m) {
            out.push(format!("{path}: longer than maxLength"));
        }
    }
    if let Some(a) = v.as_array() {
        let len = a.len() as u64;
        if s.get("minItems").and_then(Value::as_u64).is_some_and(|m| len < m) {
            out.push(format!("{path}: fewer than minItems"));
        }
        if s.get("maxItems").and_then(Value::as_u64).is_some_and(|m| len > m) {
            out.push(format!("{path}: more than maxItems"));
        }
        if let Some(items) = s.get("items") {
            for (i, x) in a.iter().enumerate() {
                check(items, x, &format!("{path}[{i}]"), out);
            }
        }
    }
    if let Some(o) = v.as_object() {
        if let Some(req) = s.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !o.contains_key(r) {
                    out.push(format!("{path}: missing required {r:?}"));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        if let Some(props) = props {
            for (k, sub) in props {
                if let Some(x) = o.get(k) {
                    check(sub, x, &format!("{path}.{k}"), out);
                }
            }
        }
        if s.get("additionalProperties") == Some(&Value::Bool(false)) {
            for k in o.keys() {
                if !props.is_some_and(|p| p.contains_key(k)) {
                    out.push(format!("{path}: unexpected property {k:?}"));
                }
            }
        }
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool() -> ToolDef {
        ToolDef {
            name: "cargo_check".into(),
            provider: "hub".into(),
            description: "check".into(),
            input_schema: json!({"type": "object", "required": ["path"], "additionalProperties": false,
                                 "properties": {"path": {"type": "string", "minLength": 1},
                                                "jobs": {"type": "integer", "minimum": 1, "maximum": 64},
                                                "mode": {"enum": ["debug", "release"]}}}),
            side_effects: "read".into(),
        }
    }

    fn run(rec: Value) -> CheckOutcome {
        let t = [tool()];
        ToolSchemaCheck.run(&CheckInput {
            record: &rec,
            tools: &t,
        })
    }

    #[test]
    fn a_valid_call_passes_and_a_bad_one_says_why() {
        let ok = run(json!({"tool_calls": [{"name": "cargo_check", "arguments": {"path": ".", "jobs": 4, "mode": "debug"}}]}));
        assert!(ok.ran && ok.ok, "{ok:?}");
        let bad = run(json!({"tool_calls": [{"name": "cargo_check", "arguments": {"jobs": 0, "mode": "fast", "x": 1}}]}));
        assert!(bad.ran && !bad.ok);
        for part in ["missing required \"path\"", "below the minimum", "not one of", "unexpected property"] {
            assert!(bad.note.contains(part), "{part} in {}", bad.note);
        }
        let unknown = run(json!({"tool_calls": [{"name": "rm_rf", "arguments": {}}]}));
        assert!(!unknown.ok && unknown.note.contains("unknown tool"));
        let wrong = run(json!({"tool_calls": [{"name": "cargo_check", "arguments": {"path": 5}}]}));
        assert!(!wrong.ok && wrong.note.contains("expected"));
    }

    #[test]
    fn records_without_calls_are_not_checked_and_the_default_is_a_no_op() {
        let r = json!({"input": [], "output": "x"});
        assert_eq!(run(r.clone()), CheckOutcome::skipped());
        let n = NoopCheck.run(&CheckInput {
            record: &r,
            tools: &[],
        });
        assert!(!n.ran && n.ok);
    }

    #[test]
    fn nested_and_array_schemas() {
        let s = json!({"type": "object", "properties": {"xs": {"type": "array", "items": {"type": "integer"}, "minItems": 1}}});
        assert!(validate(&s, &json!({"xs": [1, 2]})).is_empty());
        assert_eq!(validate(&s, &json!({"xs": [1, "a"]})).len(), 1);
        assert_eq!(validate(&s, &json!({"xs": []})).len(), 1);
    }
}
