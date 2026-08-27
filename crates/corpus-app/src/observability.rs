//! Stable structured-event contracts for application coordinators.
//!
//! Events belong at operation boundaries. UI paint functions must not emit
//! lifecycle telemetry, and subscribers remain an executable-level concern.

use std::time::Duration;

use corpus_core::Error;

use crate::state::RunId;

const LIFECYCLE_EVENT: &str = "lifecycle.operation";
const DELIVERY_EVENT: &str = "delivery.operation";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleOperation {
    LaunchAdoption,
}

impl LifecycleOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::LaunchAdoption => "launch_adoption",
        }
    }
}

pub(crate) struct LifecycleEvent<'a> {
    run_id: &'a RunId,
    operation: LifecycleOperation,
    elapsed: Duration,
    retryable: bool,
}

impl<'a> LifecycleEvent<'a> {
    pub(crate) fn new(
        run_id: &'a RunId,
        operation: LifecycleOperation,
        elapsed: Duration,
        retryable: bool,
    ) -> Self {
        Self {
            run_id,
            operation,
            elapsed,
            retryable,
        }
    }

    pub(crate) fn emit_result(self, result: &Result<(), Error>) {
        let run_session = self.run_id.storage_key();
        let elapsed_ms = u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX);
        let (outcome, error) = match result {
            Ok(()) => ("succeeded", String::new()),
            Err(error) => ("failed", error.to_string()),
        };
        tracing::info!(
            target: "corpus.lifecycle",
            event = LIFECYCLE_EVENT,
            project = self.run_id.project.as_str(),
            mission = self.run_id.mission.as_str(),
            run_session = run_session.as_str(),
            operation = self.operation.as_str(),
            generation = self.run_id.generation,
            elapsed_ms,
            outcome,
            retryable = self.retryable,
            error = error.as_str(),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryTerminal {
    Acknowledged,
    Abandoned,
    Failed,
    RetryReady,
    PersistenceFailed,
    StatusError,
}

impl DeliveryTerminal {
    fn as_str(self) -> &'static str {
        match self {
            Self::Acknowledged => "acknowledged",
            Self::Abandoned => "abandoned",
            Self::Failed => "failed",
            Self::RetryReady => "retry_ready",
            Self::PersistenceFailed => "persistence_failed",
            Self::StatusError => "status_error",
        }
    }

    fn outcome(self) -> &'static str {
        match self {
            Self::Acknowledged => "succeeded",
            Self::Abandoned => "abandoned",
            Self::Failed | Self::RetryReady | Self::PersistenceFailed | Self::StatusError => {
                "failed"
            }
        }
    }
}

pub(crate) struct DeliveryEvent<'a> {
    parent: &'a corpus_core::MissionRunRef,
    message_id: &'a str,
    attempt: u32,
    child_count: usize,
    elapsed: Duration,
}

impl<'a> DeliveryEvent<'a> {
    pub(crate) fn new(
        parent: &'a corpus_core::MissionRunRef,
        message_id: &'a str,
        attempt: u32,
        child_count: usize,
        elapsed: Duration,
    ) -> Self {
        Self {
            parent,
            message_id,
            attempt,
            child_count,
            elapsed,
        }
    }

    pub(crate) fn emit(self, terminal: DeliveryTerminal, retryable: bool, error: &str) {
        let elapsed_ms = u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX);
        let child_count = u64::try_from(self.child_count).unwrap_or(u64::MAX);
        tracing::info!(
            target: "corpus.delivery",
            event = DELIVERY_EVENT,
            project = self.parent.project.as_str(),
            mission = self.parent.mission.as_str(),
            run_session = self.parent.run_id.as_str(),
            operation = "curator_completion_delivery",
            message_id = self.message_id,
            attempt = u64::from(self.attempt),
            child_count,
            elapsed_ms,
            outcome = terminal.outcome(),
            terminal_state = terminal.as_str(),
            retryable,
            error,
        );
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{Layer, Registry};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct RecordedEvent {
        pub(crate) target: String,
        pub(crate) fields: BTreeMap<String, String>,
    }

    #[derive(Clone)]
    struct CaptureLayer(Arc<Mutex<Vec<RecordedEvent>>>);

    impl<S> Layer<S> for CaptureLayer
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);
            self.0.lock().unwrap().push(RecordedEvent {
                target: event.metadata().target().to_string(),
                fields: visitor.fields,
            });
        }
    }

    #[derive(Default)]
    struct FieldVisitor {
        fields: BTreeMap<String, String>,
    }

    impl Visit for FieldVisitor {
        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.fields
                .insert(field.name().to_string(), format!("{value:?}"));
        }
    }

    pub(crate) fn capture_events<T>(operation: impl FnOnce() -> T) -> (T, Vec<RecordedEvent>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let subscriber = Registry::default().with(CaptureLayer(events.clone()));
        let result = tracing::subscriber::with_default(subscriber, operation);
        let events = events.lock().unwrap().clone();
        (result, events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_lifecycle_event_keeps_the_stable_field_contract() {
        let run_id = RunId {
            project: "project".into(),
            mission: "mission".into(),
            generation: 7,
        };
        let (_, events) = testing::capture_events(|| {
            LifecycleEvent::new(
                &run_id,
                LifecycleOperation::LaunchAdoption,
                Duration::from_millis(23),
                false,
            )
            .emit_result(&Ok(()));
        });

        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.target, "corpus.lifecycle");
        assert_eq!(
            event.fields,
            std::collections::BTreeMap::from([
                ("elapsed_ms".into(), "23".into()),
                ("error".into(), String::new()),
                ("event".into(), "lifecycle.operation".into()),
                ("generation".into(), "7".into()),
                ("mission".into(), "mission".into()),
                ("operation".into(), "launch_adoption".into()),
                ("outcome".into(), "succeeded".into()),
                ("project".into(), "project".into()),
                ("retryable".into(), "false".into()),
                ("run_session".into(), "p7-project-m7-mission-g7".into(),),
            ])
        );
    }
}
