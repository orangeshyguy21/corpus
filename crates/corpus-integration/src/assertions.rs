//! Contract assertions shared by assembled scenarios.

use corpus_store::{Mission, MissionRunRef};

pub fn assert_exact_parent(mission: &Mission, expected: &MissionRunRef) {
    let actual = mission
        .dispatch
        .as_ref()
        .map(|dispatch| &dispatch.parent)
        .or_else(|| {
            mission
                .launch_requested
                .as_ref()
                .and_then(|request| request.requested_by.as_ref())
        });
    assert_eq!(
        actual,
        Some(expected),
        "mission lost its exact parent origin"
    );
}
