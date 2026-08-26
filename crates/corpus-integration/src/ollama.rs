//! Read-only Ollama discovery for pinned live scenarios.

use std::io;
use std::process::Command;

use serde::{Deserialize, Serialize};

pub const MODEL_ENV: &str = "CORPUS_QWEN38_MODEL";
pub const DEFAULT_MODEL: &str = "qwen3.8:27b-mlx";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OllamaModel {
    pub name: String,
    pub digest: String,
}

pub fn required_model() -> String {
    std::env::var(MODEL_ENV).unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

pub fn installed_models() -> io::Result<Vec<OllamaModel>> {
    let output = Command::new("ollama").arg("list").output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "ollama list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    parse_list(&String::from_utf8_lossy(&output.stdout))
}

pub fn require_qwen38() -> io::Result<OllamaModel> {
    let required = required_model();
    let normalized = required.to_ascii_lowercase();
    if !normalized.contains("qwen3.8") || !normalized.ends_with("-mlx") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{MODEL_ENV} must name the Qwen3.8 MLX model, got {required}"),
        ));
    }
    let available = installed_models()?;
    available
        .iter()
        .find(|model| model.name == required)
        .cloned()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "required model {required} is not installed; available: {:?}",
                    available
                        .iter()
                        .map(|model| &model.name)
                        .collect::<Vec<_>>()
                ),
            )
        })
}

fn parse_list(raw: &str) -> io::Result<Vec<OllamaModel>> {
    let mut models = Vec::new();
    for line in raw.lines().skip(1).filter(|line| !line.trim().is_empty()) {
        let mut columns = line.split_whitespace();
        let name = columns.next().unwrap_or_default();
        let digest = columns.next().unwrap_or_default();
        if name.is_empty() || digest.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unrecognized ollama list row: {line}"),
            ));
        }
        models.push(OllamaModel {
            name: name.to_string(),
            digest: digest.to_string(),
        });
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_digest_without_guessing_from_size_columns() {
        let models = parse_list(
            "NAME                ID              SIZE      MODIFIED\nqwen3.8:27b-mlx     aabbccddeeff    17 GB     2 days ago\n",
        )
        .unwrap();
        assert_eq!(
            models,
            vec![OllamaModel {
                name: "qwen3.8:27b-mlx".into(),
                digest: "aabbccddeeff".into()
            }]
        );
    }

    #[test]
    fn non_mlx_override_is_refused_before_model_discovery() {
        std::env::set_var(MODEL_ENV, "qwen3.8:27b");
        let error = require_qwen38().unwrap_err();
        std::env::remove_var(MODEL_ENV);
        assert!(error.to_string().contains("Qwen3.8 MLX"), "{error}");
    }
}
