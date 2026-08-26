use corpus_integration::TestHarness;

#[test]
fn every_scenario_receives_an_isolated_store_and_evidence_staging_area() {
    let first = TestHarness::new("isolation-a");
    let second = TestHarness::new("isolation-b");
    assert_ne!(first.world(), second.world());
    assert_ne!(first.store().root(), second.store().root());
    first.record_text("events.jsonl", "{\"event\":\"ready\"}\n");
}
