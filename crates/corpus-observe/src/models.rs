//! The model registry: open-weight models as tagged, benchmarked
//! equipment. See docs/architecture.md ("The model lab").
//!
//! Also home of `model_list()` (app-flow chunk 8): the AVAILABLE
//! models from `opencode models --verbose`, parsed, provider-grouped,
//! and TTL-cached — the app's model pickers render this.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use corpus_store::Error;

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

    /// Load the optional shipped registry independently of process cwd.
    pub fn load_default() -> Result<Self, Error> {
        Self::load(&corpus_store::paths::models_manifest()?)
    }

    /// The model the app pre-fills for a launch: the registry's first
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

// --- model_list(): opencode's available models, grouped (chunk 8) ---

/// How long the process-global model-list cache is trusted. The
/// shell-out costs ~0.6s and the app renders pickers every frame;
/// `refresh` bypasses the TTL AND re-pulls opencode's models.dev cache.
const MODEL_LIST_TTL: Duration = Duration::from_secs(300);

/// One selectable model: the opencode ref plus its real display name.
#[derive(Debug, Clone)]
pub struct ModelOption {
    /// The full ref handed to opencode: `provider/model`.
    pub id: String,
    /// The model part (after the first slash).
    pub model: String,
    /// The display name from `opencode models --verbose` (falls back
    /// to the model id when a record carries none).
    pub name: String,
}

/// One provider's models, grouped for the picker.
#[derive(Debug, Clone)]
pub struct ModelProviderGroup {
    /// The provider id, e.g. `openrouter`.
    pub id: String,
    /// The display label (`ollama` -> "Ollama (local)"; unknown
    /// providers keep the raw id).
    pub label: String,
    /// The provider's models, sorted by model id.
    pub models: Vec<ModelOption>,
}

/// The available models, grouped by provider (groups and models both
/// sorted, so every picker renders the same order).
#[derive(Debug, Clone, Default)]
pub struct ModelList {
    pub groups: Vec<ModelProviderGroup>,
}

impl ModelList {
    /// A model's display name by full id (the picker's button label).
    pub fn display_name(&self, id: &str) -> Option<&str> {
        self.groups
            .iter()
            .flat_map(|g| g.models.iter())
            .find(|m| m.id == id)
            .map(|m| m.name.as_str())
    }
}

/// `opencode models --verbose`, parsed and grouped by provider, cached
/// process-wide with a TTL. `refresh` bypasses the TTL and passes
/// opencode's own `--refresh` (re-pulls the models.dev cache; an
/// `ollama pull` shows up on the next plain call). Errors when
/// opencode is missing, errors, or returns nothing — callers degrade
/// to free text.
pub fn model_list(refresh: bool) -> Result<ModelList, Error> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Result<ModelList, String>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if !refresh {
        let hit = cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|(fetched, result)| {
                (fetched.elapsed() < MODEL_LIST_TTL).then(|| result.clone())
            });
        if let Some(result) = hit {
            return result.map_err(Error::Store);
        }
    }
    let result = pull_model_list(refresh).map_err(|error| error.to_string());
    *cache.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), result.clone()));
    result.map_err(Error::Store)
}

/// The shell-out: `opencode models --verbose [--refresh]`, parsed.
fn pull_model_list(refresh: bool) -> Result<ModelList, Error> {
    let opencode = crate::resolve_opencode()?;
    let mut command = Command::new(opencode);
    command.args(["models", "--verbose"]);
    if refresh {
        command.arg("--refresh");
    }
    let output = command
        .output()
        .map_err(|e| Error::Store(format!("opencode models failed to run: {e}")))?;
    if !output.status.success() {
        return Err(Error::Store("opencode models reported an error".into()));
    }
    let entries = parse_verbose_models(&String::from_utf8_lossy(&output.stdout));
    if entries.is_empty() {
        return Err(Error::Store("opencode models returned no models".into()));
    }
    Ok(group_models(entries))
}

/// Parse the verbose listing: each record starts at a NON-indented
/// `provider/model` line, and its display name is the depth-1
/// `"name": "..."` key of the JSON block that follows (depth-1 keys
/// are indented by exactly two spaces; nested blocks may carry their
/// own names). Records without a name fall back to the model id.
fn parse_verbose_models(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<(String, Option<String>)> = None;
    for line in text.lines() {
        let record_start =
            !line.is_empty() && !line.starts_with(char::is_whitespace) && line.contains('/');
        if record_start {
            if let Some((id, name)) = current.take() {
                out.push((
                    id.clone(),
                    name.unwrap_or_else(|| model_part(&id).to_string()),
                ));
            }
            current = Some((line.trim().to_string(), None));
            continue;
        }
        if let Some((_, name)) = current.as_mut() {
            let depth1_key = line.starts_with("  \"")
                && line.as_bytes().get(2) == Some(&b'"')
                && line.as_bytes().get(3) != Some(&b' ');
            if name.is_none() && depth1_key {
                if let Some(rest) = line.trim_start().strip_prefix("\"name\":") {
                    *name = parse_json_string_value(rest);
                }
            }
        }
    }
    if let Some((id, name)) = current.take() {
        out.push((
            id.clone(),
            name.unwrap_or_else(|| model_part(&id).to_string()),
        ));
    }
    out
}

/// Extract a JSON string's value: ` "Big Pickle",` -> `Big Pickle`.
/// Common escapes are honored; `\uXXXX` degrades to literal chars
/// (the observed names are plain text).
fn parse_json_string_value(text: &str) -> Option<String> {
    let start = text.find('"')?;
    let mut out = String::new();
    let mut escaped = false;
    for c in text[start + 1..].chars() {
        if escaped {
            out.push(match c {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// Group parsed `(id, name)` entries by provider; both levels sorted.
fn group_models(entries: Vec<(String, String)>) -> ModelList {
    let mut by_provider: BTreeMap<String, Vec<ModelOption>> = BTreeMap::new();
    for (id, name) in entries {
        let provider = id.split('/').next().unwrap_or_default().to_string();
        by_provider.entry(provider).or_default().push(ModelOption {
            model: model_part(&id).to_string(),
            id,
            name,
        });
    }
    let groups = by_provider
        .into_iter()
        .map(|(id, mut models)| {
            models.sort_by(|a, b| a.model.cmp(&b.model));
            ModelProviderGroup {
                label: provider_label(&id).to_string(),
                id,
                models,
            }
        })
        .collect();
    ModelList { groups }
}

/// The model part of a `provider/model` ref (after the FIRST slash —
/// some model ids contain slashes themselves).
fn model_part(id: &str) -> &str {
    id.split_once('/').map(|(_, model)| model).unwrap_or(id)
}

/// Provider id -> display label; unknown providers keep the raw id.
fn provider_label(id: &str) -> &str {
    match id {
        "ollama" => "Ollama (local)",
        "openrouter" => "OpenRouter",
        "opencode" => "OpenCode",
        other => other,
    }
}

// --- ollama_models(): the local Ollama server's installed models (GDK chat) ---
//
// The GDK management chat drives `goose acp` -> Ollama DIRECTLY, so its model
// picker must enumerate `ollama list` (the actual serving set), NOT opencode's
// models.dev catalog. Missions/agents keep opencode's catalog (`model_list`);
// this is the chat arm's own source.

/// `ollama list`, parsed into a single Ollama group. Only models the local
/// server has pulled are selectable for the chat. Errors when `ollama` is
/// missing or returns nothing — callers degrade to free text.
pub fn ollama_models() -> Result<ModelList, Error> {
    ollama_models_refresh(false)
}

/// Cached Ollama discovery. Both success and failure are retained so a
/// missing local server cannot turn a picker paint into a subprocess loop.
/// Explicit refresh bypasses the cache.
pub fn ollama_models_refresh(refresh: bool) -> Result<ModelList, Error> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Result<ModelList, String>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if !refresh {
        let hit = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .and_then(|(fetched, result)| {
                (fetched.elapsed() < MODEL_LIST_TTL).then(|| result.clone())
            });
        if let Some(result) = hit {
            return result.map_err(Error::Store);
        }
    }
    let result = pull_ollama_models().map_err(|error| error.to_string());
    *cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((Instant::now(), result.clone()));
    result.map_err(Error::Store)
}

fn pull_ollama_models() -> Result<ModelList, Error> {
    let ollama = std::env::var("OLLAMA").unwrap_or_else(|_| "ollama".to_string());
    let output = Command::new(&ollama)
        .arg("list")
        .output()
        .map_err(|e| Error::Store(format!("ollama list failed to run: {e}")))?;
    if !output.status.success() {
        return Err(Error::Store("ollama list reported an error".into()));
    }
    let names = parse_ollama_list(&String::from_utf8_lossy(&output.stdout));
    if names.is_empty() {
        return Err(Error::Store("ollama returned no models".into()));
    }
    let mut models: Vec<ModelOption> = names
        .into_iter()
        .map(|n| ModelOption {
            id: format!("ollama/{n}"),
            model: n.clone(),
            name: n.clone(),
        })
        .collect();
    models.sort_by(|a, b| a.model.cmp(&b.model));
    Ok(ModelList {
        groups: vec![ModelProviderGroup {
            id: "ollama".into(),
            label: "Ollama (local)".to_string(),
            models,
        }],
    })
}

/// Parse `ollama list`'s tabular output: the first whitespace token of each
/// non-header row is the model name (`NAME ID SIZE MODIFIED` header skipped).
fn parse_ollama_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.trim_start().starts_with("NAME") {
            continue;
        }
        if let Some(name) = line.split_whitespace().next() {
            out.push(name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed-down but shape-exact sample of `opencode models
    /// --verbose`: bare `provider/model` record lines, JSON blocks with
    /// a depth-1 name (and a nested name that must NOT win), one
    /// record without a name at all.
    const SAMPLE: &str = r#"opencode/big-pickle
{
  "id": "big-pickle",
  "providerID": "opencode",
  "name": "Big Pickle",
  "api": {
    "name": "nested — must not win"
  }
}
ollama/qwen3.6:35b
{
  "id": "qwen3.6:35b",
  "name": "Qwen3.6 35B (MoE)",
  "limit": {
    "context": 262144
  }
}
openrouter/~anthropic/claude-fable-latest
{
  "id": "~anthropic/claude-fable-latest"
}
"#;

    #[test]
    fn verbose_parse_extracts_ids_and_real_names() {
        let entries = parse_verbose_models(SAMPLE);
        assert_eq!(entries.len(), 3, "{entries:?}");
        assert_eq!(
            entries[0],
            ("opencode/big-pickle".to_string(), "Big Pickle".to_string())
        );
        assert_eq!(
            entries[1],
            (
                "ollama/qwen3.6:35b".to_string(),
                "Qwen3.6 35B (MoE)".to_string()
            )
        );
        // No name key -> the model id (after the first slash) is the fallback.
        assert_eq!(
            entries[2],
            (
                "openrouter/~anthropic/claude-fable-latest".to_string(),
                "~anthropic/claude-fable-latest".to_string()
            )
        );
    }

    #[test]
    fn grouping_sorts_providers_and_models_and_labels_known() {
        let list = group_models(vec![
            ("openrouter/b".to_string(), "Bee".to_string()),
            ("ollama/z".to_string(), "Zed".to_string()),
            ("ollama/a".to_string(), "Ay".to_string()),
            ("mystery/x".to_string(), "Ex".to_string()),
        ]);
        let ids: Vec<&str> = list.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["mystery", "ollama", "openrouter"]);
        let ollama = &list.groups[1];
        assert_eq!(ollama.label, "Ollama (local)");
        let model_ids: Vec<&str> = ollama.models.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(model_ids, ["a", "z"], "models sorted within the group");
        assert_eq!(
            list.groups[0].label, "mystery",
            "unknown provider keeps the raw id"
        );
        assert_eq!(list.display_name("ollama/z"), Some("Zed"));
        assert_eq!(list.display_name("nope/none"), None);
    }

    #[test]
    fn json_string_values_unescape_and_stop_at_the_closing_quote() {
        assert_eq!(
            parse_json_string_value(" \"Big Pickle\",").as_deref(),
            Some("Big Pickle")
        );
        assert_eq!(
            parse_json_string_value(" \"a \\\"quoted\\\" b\"").as_deref(),
            Some("a \"quoted\" b")
        );
        assert_eq!(parse_json_string_value(" no string here"), None);
    }

    #[test]
    fn ollama_list_parse_takes_name_column_only() {
        let sample = concat!(
            "NAME                                    ID              SIZE      MODIFIED      \n",
            "qwen3.8:27b                             0b1bb9add2f8    29 GB      2 minutes ago\n",
            "hf.co/ggml-org/Qwen3.8-27B-GGUF:Q8_0    0b1bb9add2f8    29 GB      6 minutes ago \n",
            "qwen3.6:35b                             07d35212591f    23 GB      3 months ago  \n",
        );
        let names = parse_ollama_list(sample);
        assert_eq!(
            names,
            vec![
                "qwen3.8:27b".to_string(),
                "hf.co/ggml-org/Qwen3.8-27B-GGUF:Q8_0".to_string(),
                "qwen3.6:35b".to_string(),
            ]
        );
    }

    #[test]
    fn ollama_group_labels_known_provider_and_keeps_ids() {
        let list = group_models(vec![(
            "ollama/qwen3.6:35b".to_string(),
            "qwen3.6:35b".to_string(),
        )]);
        assert_eq!(list.groups[0].label, "Ollama (local)");
    }

    #[test]
    #[ignore = "live: shells the real opencode (network on --refresh)"]
    fn live_model_list_groups_real_providers() {
        let list = model_list(true).expect("opencode models");
        assert!(!list.groups.is_empty());
        let summary: Vec<(String, String, usize)> = list
            .groups
            .iter()
            .map(|g| (g.id.clone(), g.label.clone(), g.models.len()))
            .collect();
        eprintln!("providers: {summary:?}");
        assert!(list.groups.iter().all(|g| !g.models.is_empty()));
        assert!(list
            .groups
            .iter()
            .flat_map(|g| g.models.iter())
            .all(|m| !m.name.is_empty()));
    }
}
