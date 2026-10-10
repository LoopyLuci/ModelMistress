//! The models the app shows: rows built from what the hub's `model.list` answers. The hub is the one source of
//! truth (what is on the machine, what is loaded); the app keeps no list of its own.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelRow {
    pub id: String,
    pub size_gb: f64,
    pub architecture: String,
    pub file_type: String,
    pub loaded: bool,
}

impl ModelRow {
    pub fn from_hub(v: &Value) -> Self {
        let text = |k: &str| v[k].as_str().unwrap_or("-").to_string();
        Self {
            id: v["id"].as_str().unwrap_or("").to_string(),
            size_gb: v["size_gb"].as_f64().unwrap_or(0.0),
            architecture: text("architecture"),
            file_type: text("file_type"),
            loaded: v["loaded"] == true,
        }
    }
}

pub fn rows(list: &[Value]) -> Vec<ModelRow> {
    list.iter().map(ModelRow::from_hub).collect()
}

/// The rows as text, one model a line.
pub fn table(rows: &[ModelRow]) -> String {
    if rows.is_empty() {
        return "No models found. /pull <model> downloads one.".to_string();
    }
    rows.iter()
        .map(|m| {
            format!(
                "{:<48} {:>7.2} GB  {:<12} {:<8} {}",
                m.id,
                m.size_gb,
                m.architecture,
                m.file_type,
                if m.loaded { "loaded" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_come_from_the_hubs_answer() {
        let list = vec![
            json!({"id": "llama3.2:1b", "size_gb": 1.3, "architecture": "llama", "file_type": "Q8_0", "loaded": true}),
            json!({"id": "bare"}),
        ];
        let r = rows(&list);
        assert_eq!(r[0].id, "llama3.2:1b");
        assert!(r[0].loaded && !r[1].loaded);
        assert_eq!(r[1].architecture, "-");
        let t = table(&r);
        assert!(t.contains("llama3.2:1b") && t.contains("loaded") && t.lines().count() == 2);
        assert!(table(&[]).contains("/pull"));
    }
}
