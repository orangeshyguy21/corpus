//! YAML compatibility boundary for persisted and shipped Corpus documents.
//!
//! Callers use this module instead of binding to a parser implementation.
//! `Mapping` and `Value` remain exposed because frontmatter mutation needs a
//! lossless representation; parsing, serialization, and errors stay owned by
//! this adapter so the deprecated backend can be replaced behind one seam.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::Serialize;

pub use serde_yaml::{Mapping, Value};

/// One-based source position reported by the YAML backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

/// Backend-neutral YAML failure with stable human-readable context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    message: String,
    location: Option<Location>,
}

impl Error {
    /// One-based parser location when the backend can identify one.
    pub fn location(&self) -> Option<Location> {
        self.location
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<serde_yaml::Error> for Error {
    fn from(error: serde_yaml::Error) -> Self {
        let location = error.location().map(|location| Location {
            line: location.line(),
            column: location.column(),
        });
        Self {
            message: error.to_string(),
            location,
        }
    }
}

/// Deserialize one YAML document.
pub fn from_str<T: DeserializeOwned>(source: &str) -> Result<T, Error> {
    serde_yaml::from_str(source).map_err(Error::from)
}

/// Deserialize an already parsed YAML value.
pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_yaml::from_value(value).map_err(Error::from)
}

/// Serialize a value as one YAML document.
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    serde_yaml::to_string(value).map_err(Error::from)
}

/// Convert a serializable value into the YAML representation tree.
pub fn to_value<T: Serialize>(value: T) -> Result<Value, Error> {
    serde_yaml::to_value(value).map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct CompatibilityDocument {
        name: String,
        enabled: bool,
        count: u64,
        labels: BTreeMap<String, String>,
    }

    #[test]
    fn persisted_scalars_and_nested_maps_round_trip_without_type_drift() {
        let document = CompatibilityDocument {
            name: "yes: still a string # not a comment".into(),
            enabled: true,
            count: 7,
            labels: BTreeMap::from([
                ("leading_zero".into(), "007".into()),
                ("null_word".into(), "null".into()),
                ("unicode".into(), "Qwen ✓".into()),
            ]),
        };

        let serialized = to_string(&document).unwrap();
        assert_eq!(
            serialized,
            concat!(
                "name: 'yes: still a string # not a comment'\n",
                "enabled: true\n",
                "count: 7\n",
                "labels:\n",
                "  leading_zero: '007'\n",
                "  null_word: 'null'\n",
                "  unicode: Qwen ✓\n",
            )
        );
        assert_eq!(
            from_str::<CompatibilityDocument>(&serialized).unwrap(),
            document
        );
    }

    #[test]
    fn unknown_fields_are_accepted_but_not_invented_on_reserialize() {
        let source = concat!(
            "name: corpus\n",
            "enabled: true\n",
            "count: 3\n",
            "labels: {}\n",
            "future_field:\n  nested: preserved-by-reader-only\n",
        );

        let parsed: CompatibilityDocument = from_str(source).unwrap();
        let serialized = to_string(&parsed).unwrap();

        assert_eq!(parsed.name, "corpus");
        assert!(!serialized.contains("future_field"));
    }

    #[test]
    fn malformed_documents_keep_actionable_one_based_locations() {
        let error = from_str::<CompatibilityDocument>("name: [\nenabled: true\n")
            .expect_err("malformed YAML must fail");

        assert_eq!(error.location(), Some(Location { line: 1, column: 7 }));
        assert!(error.to_string().contains("line 1 column 7"), "{error}");
    }
}
