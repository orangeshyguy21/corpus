//! Core templates and the agent-file renderer.
//!
//! The data model splits a role into three templates — permission, prompt,
//! and agent (`permission_ref` + `prompt_ref` + model) — because roles
//! differ only in prompt, tools, and model (roadmap §2): they must be
//! composable, not monolithic markdown. Execution budget is NOT part of
//! an agent: it lives on the TEAM (`TeamSpec::budget`), the launch unit.
//!
//! On second look, the model ships three template *directories* (the core
//! set under `store/templates/`, plus per-project user/plugin sets later),
//! each holding `<slug>.md` files. The renderer materializes an agent
//! template back into an opencode agent file (`.opencode/agent/<name>.md`),
//! replacing hand-editing so enforcement stays permission-file-based.
//!
//! Fidelity rule: the permission block is stored as a YAML block scalar and
//! emitted verbatim — never re-serialized — so the rendered permission
//! semantics are byte-identical to the source template.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

use crate::error::{Error, Result};
use crate::frontmatter;

/// The three template kinds, one directory each.
#[derive(Debug, Clone)]
pub struct Templates {
    pub permissions: PathBuf,
    pub prompts: PathBuf,
    pub agents: PathBuf,
}

/// A template kind, coerced to its directory name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    Permission,
    Prompt,
    Agent,
}

impl TemplateKind {
    /// The directory name for this kind.
    pub fn dir_name(self) -> &'static str {
        match self {
            TemplateKind::Permission => "permissions",
            TemplateKind::Prompt => "prompts",
            TemplateKind::Agent => "agents",
        }
    }

    /// Human label for this kind.
    pub fn label(self) -> &'static str {
        match self {
            TemplateKind::Permission => "permission",
            TemplateKind::Prompt => "prompt",
            TemplateKind::Agent => "agent",
        }
    }
}

impl Templates {
    /// A template tree rooted at a directory containing the three kinds.
    pub fn at(root: &Path) -> Self {
        Self {
            permissions: root.join("permissions"),
            prompts: root.join("prompts"),
            agents: root.join("agents"),
        }
    }

    /// Create the three kind directories.
    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.permissions)?;
        fs::create_dir_all(&self.prompts)?;
        fs::create_dir_all(&self.agents)?;
        Ok(())
    }

    /// The directory holding one kind's templates.
    pub fn kind_dir(&self, kind: TemplateKind) -> PathBuf {
        match kind {
            TemplateKind::Permission => self.permissions.clone(),
            TemplateKind::Prompt => self.prompts.clone(),
            TemplateKind::Agent => self.agents.clone(),
        }
    }

    /// Template slugs (<stem>.md files) present in this tree for a kind.
    pub fn list(&self, kind: TemplateKind) -> Vec<String> {
        let mut slugs = Vec::new();
        let dir = self.kind_dir(kind);
        let Ok(read) = fs::read_dir(&dir) else {
            return slugs;
        };
        for entry in read.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    slugs.push(stem.to_string());
                }
            }
        }
        slugs.sort();
        slugs
    }

    /// True when a slug exists in this tree for a kind.
    pub fn has(&self, kind: TemplateKind, slug: &str) -> bool {
        self.kind_dir(kind).join(format!("{slug}.md")).is_file()
    }
}

/// A permission template: the role's opencode permission block, kept
/// verbatim as a block scalar so rendering is byte-faithful. The block is
/// validated as YAML on load so a malformed permission can never slip
/// through enforcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionTemplate {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Raw permission block, e.g. `"bash: deny\nedit: deny\n..."`.
    pub permission: String,
}

impl PermissionTemplate {
    pub fn load(dir: &Path, slug: &str) -> Result<Self> {
        let path = dir.join(format!("{slug}.md"));
        let raw = fs::read_to_string(&path).map_err(|e| Error::Store(e.to_string()))?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let (fm, _body) = frontmatter::split(raw)?;
        let fm = fm.ok_or_else(|| Error::Store("permission template has no frontmatter".into()))?;
        let name = frontmatter::get_str(&fm, "name")
            .ok_or_else(|| Error::Store("permission template missing name".into()))?;
        let description = frontmatter::get_str(&fm, "description").unwrap_or_default();
        let permission = fm
            .get(serde_yaml::Value::String("permission".into()))
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Store("permission template missing permission block".into()))?
            .to_string();
        // The block must itself be valid YAML — a permission isn't enforced
        // until it parses.
        let _: serde_yaml::Mapping = serde_yaml::from_str(&permission).map_err(|e| {
            Error::Store(format!("permission block in {name} is not valid YAML: {e}"))
        })?;
        Ok(Self {
            name,
            description,
            permission,
        })
    }
}

/// A prompt template: the role's system-prompt body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplate {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The system-prompt body, byte-for-byte what lands in the agent file.
    pub body: String,
}

impl PromptTemplate {
    pub fn load(dir: &Path, slug: &str) -> Result<Self> {
        let path = dir.join(format!("{slug}.md"));
        let raw = fs::read_to_string(&path).map_err(|e| Error::Store(e.to_string()))?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let (fm, body) = frontmatter::split(raw)?;
        let fm = fm.ok_or_else(|| Error::Store("prompt template has no frontmatter".into()))?;
        let name = frontmatter::get_str(&fm, "name")
            .ok_or_else(|| Error::Store("prompt template missing name".into()))?;
        let description = frontmatter::get_str(&fm, "description").unwrap_or_default();
        if body.trim().is_empty() {
            return Err(Error::Store(format!("prompt template {name} has an empty body")));
        }
        Ok(Self {
            name,
            description,
            body: body.to_string(),
        })
    }
}

/// An agent template: `permission_ref + prompt_ref + model`.
///
/// Rendering resolves the two refs (local templates first, core templates as
/// fallback) and emits an opencode agent file whose frontmatter carries the
/// resolved permission block and whose body is the resolved prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTemplate {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// opencode agent mode: `primary` | `subagent` | `all`.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Slug of the permission template to compose.
    pub permission_ref: String,
    /// Slug of the prompt template to compose.
    pub prompt_ref: String,
    /// Default model (`provider/model`). Empty = the operator's choice at
    /// run time, so the renderer omits it.
    #[serde(default)]
    pub model: Option<String>,
    // Budget deliberately does NOT live on the agent template: it is a
    // TEAM property (`TeamSpec::budget`), because the team is the launch
    // unit. Older template files may carry an inert `budget:` key; serde
    // skips unknown fields, so they still parse.
}

fn default_mode() -> String {
    "primary".to_string()
}

impl AgentTemplate {
    pub fn load(dir: &Path, slug: &str) -> Result<Self> {
        let path = dir.join(format!("{slug}.md"));
        let raw = fs::read_to_string(&path).map_err(|e| Error::Store(e.to_string()))?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let (fm, _body) = frontmatter::split(raw)?;
        let fm = fm.ok_or_else(|| Error::Store("agent template has no frontmatter".into()))?;
        let template: AgentTemplate =
            serde_yaml::from_str(
                &serde_yaml::to_string(&fm)
                    .map_err(|e| Error::Store(format!("agent frontmatter: {e}")))?,
            )
            .map_err(|e| Error::Store(format!("agent template: {e}")))?;
        if template.permission_ref.is_empty() {
            return Err(Error::Store("agent template missing permission_ref".into()));
        }
        if template.prompt_ref.is_empty() {
            return Err(Error::Store("agent template missing prompt_ref".into()));
        }
        Ok(template)
    }

    /// Render this agent template into `dest` (an opencode agent file).
    ///
    /// Refs resolve against `local` first, `core` as fallback — user and
    /// plugin templates (project scope) may shadow the core set. `model`
    /// overrides the template default; when both are empty the rendered
    /// file carries no model (operator's choice at run time).
    pub fn render(
        &self,
        local: &Templates,
        core: &Templates,
        model: Option<&str>,
        dest: &Path,
    ) -> Result<()> {
        let perm = resolve_permission(local, core, &self.permission_ref)?;
        let prompt = resolve_prompt(local, core, &self.prompt_ref)?;
        let rendered = compose_agent_file(self, &perm.permission, &prompt.body, model);
        fs::write(dest, rendered)?;
        Ok(())
    }
}

/// Resolve a permission ref against local then core template sets.
fn resolve_permission(local: &Templates, core: &Templates, slug: &str) -> Result<PermissionTemplate> {
    if local.permissions.join(format!("{slug}.md")).is_file() {
        return PermissionTemplate::load(&local.permissions, slug);
    }
    PermissionTemplate::load(&core.permissions, slug)
}

/// Resolve a prompt ref against local then core template sets.
fn resolve_prompt(local: &Templates, core: &Templates, slug: &str) -> Result<PromptTemplate> {
    if local.prompts.join(format!("{slug}.md")).is_file() {
        return PromptTemplate::load(&local.prompts, slug);
    }
    PromptTemplate::load(&core.prompts, slug)
}

// -------------------------------------------------------------------------
// Template authoring (chunk 4a): compose files, validate, resolve refs.
// -------------------------------------------------------------------------

/// Validate a permission block alone (validate-on-save and the deck
/// editor's inline check). A permission isn't enforced until it parses.
pub fn validate_permission_block(block: &str) -> Result<()> {
    let _: Mapping = serde_yaml::from_str(block)
        .map_err(|e| Error::Store(format!("permission block is not valid YAML: {e}")))?;
    Ok(())
}

/// True when a permission ref resolves project-then-core.
pub fn permission_resolves(local: &Templates, core: &Templates, slug: &str) -> bool {
    local.permissions.join(format!("{slug}.md")).is_file()
        || core.permissions.join(format!("{slug}.md")).is_file()
}

/// True when a prompt ref resolves project-then-core.
pub fn prompt_resolves(local: &Templates, core: &Templates, slug: &str) -> bool {
    local.prompts.join(format!("{slug}.md")).is_file()
        || core.prompts.join(format!("{slug}.md")).is_file()
}

fn y(s: &str) -> Value {
    Value::String(s.to_string())
}

/// Compose a permission template file: frontmatter (the `permission`
/// block serialized as a YAML block scalar) plus a human-notes body.
pub fn compose_permission_template(t: &PermissionTemplate) -> Result<String> {
    let mut map = Mapping::new();
    map.insert(y("name"), y(&t.name));
    map.insert(y("description"), y(&t.description));
    map.insert(y("permission"), y(&t.permission));
    compose_frontmatter(map)
}

/// Compose a prompt template file: frontmatter + the prompt body.
pub fn compose_prompt_template(t: &PromptTemplate) -> Result<String> {
    let mut map = Mapping::new();
    map.insert(y("name"), y(&t.name));
    map.insert(y("description"), y(&t.description));
    let fm = compose_frontmatter(map)?;
    Ok(format!("{fm}\n{}", t.body))
}

/// Compose an agent template file: frontmatter only (a composer has no
/// body). Empty model serializes as null and parses back to None.
pub fn compose_agent_template(t: &AgentTemplate) -> Result<String> {
    let mut map = Mapping::new();
    map.insert(y("name"), y(&t.name));
    map.insert(y("description"), y(&t.description));
    map.insert(y("mode"), y(&t.mode));
    map.insert(y("permission_ref"), y(&t.permission_ref));
    map.insert(y("prompt_ref"), y(&t.prompt_ref));
    map.insert(
        y("model"),
        t.model.as_deref().map(y).unwrap_or(Value::Null),
    );
    compose_frontmatter(map)
}

/// Serialize a frontmatter mapping into a `---`-fenced file.
fn compose_frontmatter(map: Mapping) -> Result<String> {
    let fm = serde_yaml::to_string(&map)
        .map_err(|e| Error::Store(format!("cannot serialize template: {e}")))?;
    Ok(format!("---\n{fm}---\n"))
}

/// Compose a rendered opencode agent file.
///
/// The output is deliberately assembled by hand (not round-tripped through
/// a YAML serializer): the permission block is spliced verbatim and the
/// description/mode are plain scalars, so a byte-diff against a
/// hand-maintained agent file is read: description, mode, (model), the
/// permission block, then the prompt body.
fn compose_agent_file(
    agent: &AgentTemplate,
    permission_block: &str,
    prompt_body: &str,
    model: Option<&str>,
) -> String {
    let mut out = String::with_capacity(permission_block.len() + prompt_body.len() + 128);
    out.push_str("---\n");
    out.push_str("description: ");
    out.push_str(&agent.description);
    out.push('\n');
    out.push_str("mode: ");
    out.push_str(&agent.mode);
    out.push('\n');
    let model = model
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .or_else(|| agent.model.as_deref().map(str::trim).filter(|m| !m.is_empty()));
    if let Some(model) = model {
        out.push_str("model: ");
        out.push_str(model);
        out.push('\n');
    }
    out.push_str("permission:\n");
    for line in permission_block.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("---\n");
    // The prompt body retains its leading blank separator line; appending it
    // here keeps the rendered file byte-identical to a hand-maintained one.
    out.push_str(prompt_body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPERATOR_PERM: &str = "bash: deny\nedit: deny\nwrite: deny\nread: deny\n";

    fn write_template(root: &Path, kind: &str, slug: &str, content: &str) {
        let dir = root.join(kind);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{slug}.md")), content).unwrap();
    }

    fn sample_templates(root: &Path) -> Templates {
        let permission = format!(
            "---\nname: op-perm\ndescription: d\npermission: |\n{}\n---\n\nnotes\n",
            OPERATOR_PERM
                .lines()
                .map(|l| format!("  {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        write_template(root, "permissions", "op-perm", &permission);
        write_template(
            root,
            "prompts",
            "op-prompt",
            "---\nname: op-prompt\ndescription: d\n---\n\nYou are a test.\n",
        );
        write_template(
            root,
            "agents",
            "op",
            "---\nname: op\ndescription: Test operator\nmode: primary\npermission_ref: op-perm\nprompt_ref: op-prompt\n---\n",
        );
        Templates::at(root)
    }

    #[test]
    fn renders_agent_file_with_verbatim_permission_block() {
        let tmp = std::env::temp_dir().join(format!("corpus-tpl-{}", std::process::id()));
        sample_templates(&tmp);
        let local = sample_templates(&tmp);
let agent = AgentTemplate::load(&local.agents, "op").unwrap();
        let dest = tmp.join("agent-op.md");
        agent.render(&local, &local, None, &dest).unwrap();
        let rendered = fs::read_to_string(&dest).unwrap();
        let indented = OPERATOR_PERM
            .lines()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        let expected = format!(
            "---\ndescription: Test operator\nmode: primary\npermission:\n{indented}\n---\n\nYou are a test.\n"
        );
        assert_eq!(rendered, expected);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_malformed_permission_block() {
        let tmp = std::env::temp_dir().join(format!("corpus-tpl-bad-{}", std::process::id()));
        write_template(
            &tmp,
            "permissions",
            "bad",
            "---\nname: bad\ndescription: d\npermission: |\n  }]] not yaml\n---\n",
        );
        let err = PermissionTemplate::load(&Templates::at(&tmp).permissions, "bad").unwrap_err();
        assert!(err.to_string().contains("not valid YAML"));
        let _ = fs::remove_dir_all(&tmp);
    }
}