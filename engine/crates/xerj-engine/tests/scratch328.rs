use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

#[tokio::test]
async fn probe_lexical_on_vector_field() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).unwrap();

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    let mut emb = FieldConfig::new("emb", FieldType::Vector);
    emb.options.dimensions = Some(4);
    emb.options.similarity = Some("cosine".to_string());
    schema.fields.push(emb);
    engine.create_index("p", schema).unwrap();
    let idx = engine.get_index("p").unwrap();

    for d in 0..5u32 {
        idx.index_document(
            Some(format!("d{d}")),
            json!({"body": format!("doc {d}"), "emb": [0.4288 + d as f64, 0.5, 0.25, 0.125]}),
        )
        .await
        .unwrap();
    }

    let run = |body: serde_json::Value| {
        let idx = idx.clone();
        async move {
            let req = parse_request(&body).unwrap();
            match idx.search(&req).await {
                Ok(r) => format!("total={}", r.total.value),
                Err(e) => format!("ERR {e}"),
            }
        }
    };

    for phase in ["memtable", "flushed"] {
        if phase == "flushed" {
            idx.refresh().await.unwrap();
            idx.force_merge(1).await.unwrap();
        }
        for q in [
            json!({"query":{"term":{"emb":0.4288}},"size":10}),
            json!({"query":{"term":{"emb":"0.4288"}},"size":10}),
            json!({"query":{"match":{"emb":"0.4288"}},"size":10}),
            json!({"query":{"match":{"emb":"0.5"}},"size":10}),
            json!({"query":{"query_string":{"query":"0.4288"}},"size":10}),
            json!({"query":{"query_string":{"query":"0.5"}},"size":10}),
            json!({"query":{"query_string":{"query":"0.25"}},"size":10}),
            json!({"query":{"simple_query_string":{"query":"0.5"}},"size":10}),
            json!({"query":{"multi_match":{"query":"0.5","fields":["*"]}},"size":10}),
            json!({"query":{"multi_match":{"query":"0.4288","fields":["*"]}},"size":10}),
            json!({"query":{"multi_match":{"query":"0.4288","fields":["emb","body"]}},"size":10}),
            json!({"query":{"exists":{"field":"emb"}},"size":10}),
            json!({"query":{"range":{"emb":{"gte":0.0}}},"size":10}),
            json!({"query":{"terms":{"emb":[0.4288]}},"size":10}),
        ] {
            println!("{phase}\t{}\t{}", run(q.clone()).await, q["query"]);
        }
    }
}
