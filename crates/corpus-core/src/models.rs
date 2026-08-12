//! The model registry: open-weight models as tagged, benchmarked
//! equipment. See docs/architecture.md ("The model lab").

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// One model in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    /// Registry tag, e.g. `qwen3.6:35b`.
    pub tag: String,
    /// Human-readable name.
    pub name: Option<String>,
    /// Provider / serving stack, e.g. `ollama`.
    #[serde(default)]
    pub provider: String,
    /// Approximate parameter count in billions, if known.
    #[serde(default)]
    pub params_b: Option<f64>,
    /// Served context window.
    #[serde(default)]
    pub context_k: Option<u32>,
    /// Capability tags, e.g. `coding`, `tool-use`, `long-context`.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Free-form notes (strengths, failure modes observed in runs).
    #[serde(default)]
    pub notes: String,
}

/// The model registry (parsed from `benchmarks/models.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRegistry {
    /// All tracked models.
    pub models: Vec<ModelEntry>,
}

impl ModelRegistry {
    /// Load from a YAML file; an absent file yields an empty registry.
    pub fn load(path: &Path) -> Result<Self, Error> {
        if !path.is_file() {
            return Ok(Self { models: Vec::new() });
        }
        let raw = fs::read_to_string(path)?;
        let registry: Self = serde_yaml::from_str(&raw)?;
        Ok(registry)
    }

    /// The model the deck pre-fills for a launch: the registry's first
    /// tool-use-capable entry (falling back to the first tracked model).
    /// This is an EXPLICIT launch arg — it pre-fills the operator's
    /// field, it is never an ambient fallback the engine picks.
    pub fn launch_default(&self) -> Option<String> {
        let entry = self
            .models
            .iter()
            .find(|m| m.capabilities.iter().any(|c| c == "tool-use"))
            .or_else(|| self.models.first())?;
        Some(format!("{}/{}", entry.provider, entry.tag))
    }
}
