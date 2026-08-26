use std::process::Command;
use std::time::Duration;

use corpus_integration::{preflight, process, ModelLease, TestHarness};

/// Minimal Tier B runner proof. The embedded management-chat scenarios use
/// the same machine-wide lease; this test verifies the configured digest and
/// captures one bounded real inference before those longer workflows run.
#[test]
#[ignore = "model-qwen38: requires the prepared local Ollama runner"]
fn configured_qwen38_model_runs_under_the_global_lease() {
    let mut harness = TestHarness::new("qwen38-preflight");
    let _lease = ModelLease::acquire(harness.scenario()).unwrap();
    let preflight = preflight::live_qwen38().unwrap_or_else(|error| {
        let path = harness.preserve_failure(&format!("preflight failed: {error}"));
        panic!("Qwen3.8 preflight failed; artifacts: {}", path.display());
    });
    harness.record_json("manifest.json", &preflight);

    let mut command = Command::new("ollama");
    command.args([
        "run",
        &preflight.model.name,
        "Reply with a short acknowledgement that the Corpus integration runner is ready.",
    ]);
    let output =
        process::json_lines(command, &[], Duration::from_secs(300)).unwrap_or_else(|error| {
            let path = harness.preserve_failure(&format!("inference failed: {error}"));
            panic!("Qwen3.8 inference failed; artifacts: {}", path.display());
        });
    harness.record_text("transcript.md", &String::from_utf8_lossy(&output.stdout));
    harness.record_text("ollama.log", &String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "ollama exited with {}",
        output.status
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "Qwen3.8 returned no response"
    );
}
