use std::fs;
use std::path::Path;

use super::*;
use crate::store::Store;

fn tmp_store(tag: &str) -> Store {
    let world =
        std::env::temp_dir().join(format!("corpus-accounting-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&world);
    Store::new(world.join("store"))
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn corpus_cost_aggregates_exports_per_model() {
    let store = tmp_store("cost");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    let export = |cost: f64, input: u64, model: &str| {
        serde_json::json!({
            "info": {},
            "messages": [{"info": {
                "role": "assistant",
                "providerID": "openrouter",
                "modelID": model,
                "cost": cost,
                "tokens": {
                    "input": input,
                    "output": 7,
                    "reasoning": 1,
                    "cache": {"read": 3, "write": 0}
                }
            }}]
        })
        .to_string()
    };
    let runs = store.project_corpus_dir("p").join("runs");
    write(
        &runs.join("1-operator.json"),
        &export(0.5, 100, "deepseek/deepseek-v4-flash"),
    );
    write(
        &runs.join("2-operator.json"),
        &export(0.25, 50, "deepseek/deepseek-v4-flash"),
    );
    write(
        &runs.join("3-operator.json"),
        &export(1.5, 200, "moonshotai/kimi-k3"),
    );
    write(&runs.join("4-operator.log"), "not json — skipped");
    write(&runs.join("5-operator.json"), "{corrupt");
    let report = corpus_cost(&store, "p").unwrap();
    assert_eq!(report.rows.len(), 2);
    // Cost-desc order: kimi first.
    assert_eq!(report.rows[0].model, "kimi-k3");
    assert_eq!(report.rows[0].provider, "openrouter");
    assert!((report.rows[0].cost - 1.5).abs() < 1e-9);
    assert_eq!(report.rows[1].messages, 2);
    assert_eq!(report.rows[1].tokens_input, 150);
    assert_eq!(report.rows[1].cache_read, 6);
    assert!((report.cost - 2.25).abs() < 1e-9);
    assert_eq!(report.tokens, 108 + 58 + 208);
    assert!(corpus_cost(&store, "ghost").unwrap().rows.is_empty());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn corpus_cost_measures_inference_time_without_parallel_tool_time() {
    let store = tmp_store("cost-inference-time");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    let export = serde_json::json!({
        "messages": [{
            "info": {
                "role": "assistant",
                "providerID": "ollama",
                "modelID": "qwen/qwen3",
                "time": {"created": 1_000, "completed": 6_000},
                "tokens": {"input": 10, "output": 5}
            },
            "parts": [
                {"type": "tool", "state": {"time": {"start": 3_000, "end": 4_000}}},
                {"type": "tool", "state": {"time": {"start": 3_500, "end": 4_500}}},
                {"type": "tool", "state": {"time": {"start": 5_000, "end": 5_250}}}
            ]
        }, {
            "info": {
                "role": "assistant",
                "providerID": "ollama",
                "modelID": "qwen/qwen3",
                "tokens": {"input": 10, "output": 5}
            }
        }]
    });
    let runs = store.project_corpus_dir("p").join("runs");
    write(&runs.join("timed.json"), &export.to_string());

    let report = corpus_cost(&store, "p").unwrap();
    // 5,000ms assistant span - 1,500ms overlapping tool union - 250ms tool.
    assert_eq!(report.inference_ms, 3_250);
    assert_eq!(report.timed_messages, 1);
    assert_eq!(report.rows[0].inference_ms, 3_250);
    assert_eq!(report.rows[0].timed_messages, 1);
    assert_eq!(report.rows[0].messages, 2);
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn corpus_cost_reexport_overwrites_not_doubles() {
    // A live conversation is re-exported every turn to the SAME
    // session-keyed file (`runs/<session-id>.json`), so its cumulative
    // usage must REPLACE the prior read, never stack on top of it.
    let store = tmp_store("cost-reexport");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    let export = |input: u64| {
        serde_json::json!({
            "info": {},
            "messages": [{"info": {
                "role": "assistant",
                "providerID": "ollama",
                "modelID": "qwen/qwen3",
                "cost": 0.0,
                "tokens": {"input": input, "output": 0,
                           "reasoning": 0, "cache": {"read": 0, "write": 0}}
            }}]
        })
        .to_string()
    };
    let runs = store.project_corpus_dir("p").join("runs");
    let file = runs.join("ses_abc.json");
    // Turn 1.
    write(&file, &export(100));
    let r1 = corpus_cost(&store, "p").unwrap();
    assert_eq!(r1.tokens, 100);
    assert_eq!(r1.rows.len(), 1);
    // Turn 2: same session, cumulative totals, same filename → overwrite.
    write(&file, &export(250));
    let r2 = corpus_cost(&store, "p").unwrap();
    assert_eq!(r2.tokens, 250, "re-export overwrote — must not be 100+250");
    assert_eq!(r2.rows.len(), 1);
    assert_eq!(r2.rows[0].tokens_input, 250);
    let _ = fs::remove_dir_all(store.root());
}
