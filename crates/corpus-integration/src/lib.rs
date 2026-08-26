//! Hermetic and serial live-model integration-test harness.
//!
//! This crate owns isolated stores, cross-process model serialization, process
//! deadlines, and failure artifacts. It contains no production application
//! behavior: scenarios must exercise Corpus crates or binaries through their
//! real public seams.

pub mod artifacts;
pub mod assertions;
pub mod harness;
pub mod model_lock;
pub mod ollama;
pub mod preflight;
pub mod process;

pub use harness::TestHarness;
pub use model_lock::ModelLease;
