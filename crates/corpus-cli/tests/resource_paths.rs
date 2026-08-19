use std::fs;
use std::process::Command;

#[test]
fn models_list_uses_the_resource_root_outside_the_workspace_cwd() {
    let fixture =
        std::env::temp_dir().join(format!("corpus-model-resources-{}", std::process::id()));
    let cwd = fixture.join("elsewhere");
    fs::create_dir_all(fixture.join("plugins")).unwrap();
    fs::create_dir_all(fixture.join("benchmarks")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::write(fixture.join("sources.toml"), "[sources]\n").unwrap();
    fs::write(
        fixture.join("benchmarks/models.yaml"),
        "models:\n  - tag: fixture-model\n    provider: fixture-provider\n    capabilities: [tool-use]\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_corpus"))
        .args(["models", "list"])
        .current_dir(&cwd)
        .env("CORPUS_RESOURCES", &fixture)
        .env_remove("CORPUS_MODELS")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("fixture-model"), "{stdout}");
    assert!(stdout.contains("fixture-provider"), "{stdout}");
    let _ = fs::remove_dir_all(&fixture);
}
