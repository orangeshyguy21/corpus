//! Global serialization for local-model inference.

pub use corpus_model_test::{ModelLease, MODEL_LOCK_ENV};

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

    #[test]
    fn a_second_lease_cannot_enter_the_model_boundary() {
        let path = std::env::temp_dir().join(format!(
            "corpus-model-lock-test-{}-{}",
            std::process::id(),
            crate::harness::unique_id()
        ));
        std::env::set_var(MODEL_LOCK_ENV, &path);
        let first = ModelLease::try_acquire("first").unwrap();
        let second = ModelLease::try_acquire("second").unwrap_err();
        assert_eq!(second.kind(), ErrorKind::WouldBlock);
        drop(first);
        ModelLease::try_acquire("third").unwrap();
        std::env::remove_var(MODEL_LOCK_ENV);
        let _ = std::fs::remove_file(path);
    }
}
