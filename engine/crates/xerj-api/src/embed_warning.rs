//! Warnings when a mapping asks for an embedder the server will not use.
//!
//! `semantic_text` accepts `inference_id`, `model_id`, and `inference_endpoint`
//! because those keys exist on the Elasticsearch wire. Ingest does not read
//! them: every document is embedded with the server's `--embed-mode`, which is
//! fixed for the index at ingest time. Saying so on the create/mapping
//! response is the warning; the hard refusal when a restarted process opens a
//! populated index under a different embedder stays in the engine.

use serde_json::{Map, Value};

/// The embedder this process will actually run. Names are compared
/// case-insensitively after trimming.
#[derive(Debug, Clone)]
pub struct ResolvedEmbedder {
    pub backend: String,
    pub neural_model: String,
    pub proxy_model: String,
    pub proxy_endpoint: String,
}

/// One warning per `semantic_text` field whose requested id or endpoint does
/// not match [`ResolvedEmbedder`]. No request is not a mismatch.
pub fn semantic_embedder_warnings(body: &Value, resolved: &ResolvedEmbedder) -> Vec<String> {
    let Some(properties) = properties_of(body) else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    collect(properties, "", &mut fields);
    let mut warnings = Vec::new();
    for (name, spec) in fields {
        if let Some(warning) = field_warning(&name, spec, resolved) {
            warnings.push(warning);
            if warnings.len() == 8 {
                warnings.push(
                    "further semantic_text embedder mismatches were omitted; the mapping does not \
                     switch the embedder"
                        .to_string(),
                );
                break;
            }
        }
    }
    warnings
}

fn properties_of(body: &Value) -> Option<&Map<String, Value>> {
    body.get("mappings")
        .and_then(|mappings| mappings.get("properties"))
        .or_else(|| body.get("properties"))
        .and_then(Value::as_object)
}

fn collect<'a>(
    properties: &'a Map<String, Value>,
    prefix: &str,
    out: &mut Vec<(String, &'a Map<String, Value>)>,
) {
    for (name, spec) in properties {
        let Some(spec) = spec.as_object() else {
            continue;
        };
        let full = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if spec.get("type").and_then(Value::as_str) == Some("semantic_text") {
            out.push((full.clone(), spec));
        }
        if let Some(nested) = spec.get("properties").and_then(Value::as_object) {
            collect(nested, &full, out);
        }
    }
}

fn field_warning(
    field: &str,
    spec: &Map<String, Value>,
    resolved: &ResolvedEmbedder,
) -> Option<String> {
    let inference_id = spec
        .get("inference_id")
        .or_else(|| spec.get("model_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let endpoint = spec
        .get("inference_endpoint")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty());
    if inference_id.is_none() && endpoint.is_none() {
        return None;
    }
    let id_mismatch = inference_id.is_some_and(|id| !id_matches(id, resolved));
    let endpoint_mismatch = endpoint.is_some_and(|endpoint| !endpoint_matches(endpoint, resolved));
    if !id_mismatch && !endpoint_mismatch {
        return None;
    }
    let mut asked = Vec::new();
    if let Some(id) = inference_id {
        if id_mismatch {
            asked.push(format!("inference_id/model_id={id}"));
        }
    }
    if let Some(endpoint) = endpoint {
        if endpoint_mismatch {
            asked.push(format!("inference_endpoint={endpoint}"));
        }
    }
    Some(format!(
        "field [{field}] sets {}, but that does not switch the embedder. This index embeds with \
         the server's {} backend, fixed at ingest time. Changing --embed-mode or this mapping \
         does not re-embed documents already written; re-index into a new index. The faster \
         default is lexical (`xerj --embed-mode lexical`, or omit the flag).",
        asked.join(" and "),
        resolved.backend
    ))
}

fn id_matches(id: &str, resolved: &ResolvedEmbedder) -> bool {
    if id.eq_ignore_ascii_case(resolved.backend.trim()) {
        return true;
    }
    match resolved.backend.trim() {
        "neural" => {
            let model = resolved.neural_model.trim();
            !model.is_empty() && id.eq_ignore_ascii_case(model)
        }
        "proxy" => {
            let model = resolved.proxy_model.trim();
            !model.is_empty() && id.eq_ignore_ascii_case(model)
        }
        _ => false,
    }
}

fn endpoint_matches(endpoint: &str, resolved: &ResolvedEmbedder) -> bool {
    if resolved.backend.trim() != "proxy" {
        return false;
    }
    let configured = resolved.proxy_endpoint.trim();
    !configured.is_empty() && endpoint.eq_ignore_ascii_case(configured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lexical() -> ResolvedEmbedder {
        ResolvedEmbedder {
            backend: "lexical".into(),
            neural_model: "sentence-transformers/all-MiniLM-L6-v2".into(),
            proxy_model: String::new(),
            proxy_endpoint: String::new(),
        }
    }

    #[test]
    fn an_empty_create_response_omits_warnings() {
        let encoded =
            serde_json::to_string(&crate::responses::EsIndexResponse::ok("docs")).unwrap();
        assert!(!encoded.contains("warnings"), "{encoded}");
        assert!(encoded.contains("\"acknowledged\":true"), "{encoded}");
        assert!(encoded.contains("\"index\":\"docs\""), "{encoded}");
    }

    #[test]
    fn a_matching_or_absent_request_is_silent() {
        let body = json!({"mappings": {"properties": {
            "body": {"type": "semantic_text"},
            "title": {"type": "semantic_text", "inference_id": "lexical"}
        }}});
        assert!(semantic_embedder_warnings(&body, &lexical()).is_empty());
    }

    #[test]
    fn a_different_inference_id_warns_and_names_reindex() {
        let body = json!({"properties": {
            "body": {"type": "semantic_text", "model_id": ".elser_model_2"}
        }});
        let warnings = semantic_embedder_warnings(&body, &lexical());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let warning = &warnings[0];
        assert!(warning.contains("body"), "{warning}");
        assert!(warning.contains(".elser_model_2"), "{warning}");
        assert!(
            warning.contains("does not switch the embedder"),
            "{warning}"
        );
        assert!(warning.contains("fixed at ingest"), "{warning}");
        assert!(warning.contains("re-index"), "{warning}");
        assert!(warning.contains("lexical"), "{warning}");
    }

    #[test]
    fn an_endpoint_on_a_non_proxy_backend_warns() {
        let body = json!({"mappings": {"properties": {
            "body": {"type": "semantic_text", "inference_endpoint": "https://example.test/v1"}
        }}});
        let warnings = semantic_embedder_warnings(&body, &lexical());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("inference_endpoint=https://example.test/v1"));
    }

    #[test]
    fn neural_accepts_its_model_id_and_rejects_another() {
        let resolved = ResolvedEmbedder {
            backend: "neural".into(),
            neural_model: "sentence-transformers/all-MiniLM-L6-v2".into(),
            proxy_model: String::new(),
            proxy_endpoint: String::new(),
        };
        let same = json!({"mappings": {"properties": {
            "body": {"type": "semantic_text", "inference_id": "Sentence-Transformers/all-MiniLM-L6-v2"}
        }}});
        assert!(semantic_embedder_warnings(&same, &resolved).is_empty());
        let other = json!({"mappings": {"properties": {
            "body": {"type": "semantic_text", "inference_id": "lexical"}
        }}});
        assert_eq!(semantic_embedder_warnings(&other, &resolved).len(), 1);
    }

    #[test]
    fn proxy_matches_its_endpoint_and_model_only() {
        let resolved = ResolvedEmbedder {
            backend: "proxy".into(),
            neural_model: String::new(),
            proxy_model: "text-embedding-3-small".into(),
            proxy_endpoint: "https://embed.example/v1".into(),
        };
        let same = json!({"mappings": {"properties": {
            "body": {
                "type": "semantic_text",
                "inference_id": "text-embedding-3-small",
                "inference_endpoint": "https://embed.example/v1"
            }
        }}});
        assert!(semantic_embedder_warnings(&same, &resolved).is_empty());
        let other = json!({"mappings": {"properties": {
            "body": {
                "type": "semantic_text",
                "inference_id": "text-embedding-3-small",
                "inference_endpoint": "https://other.example/v1"
            }
        }}});
        let warnings = semantic_embedder_warnings(&other, &resolved);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("other.example"));
        assert!(!warnings[0].contains("inference_id/model_id"));
    }
}
