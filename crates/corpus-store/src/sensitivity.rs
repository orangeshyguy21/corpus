//! Sensitivity classes for persisted corpus entries.
//!
//! Every store entity carries `sensitivity: open | internal | embargoed` in
//! its frontmatter, defaulted by the MCP write tools at creation (embargoed
//! for findings, internal otherwise). Policy gates act on the class — the
//! only gate in this scope is the promotion gate: an entry may not leave a
//! team scope without explicit operator confirmation while it is embargoed.

/// The store sensitivity classes, ordered from least to most restrictive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sensitivity {
    /// Safe for public view.
    Open,
    /// Internal to the team/project.
    Internal,
    /// A verified, undisclosed artifact (crown jewels).
    Embargoed,
}

impl Sensitivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Sensitivity::Open => "open",
            Sensitivity::Internal => "internal",
            Sensitivity::Embargoed => "embargoed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Sensitivity::Open),
            "internal" => Some(Sensitivity::Internal),
            "embargoed" => Some(Sensitivity::Embargoed),
            _ => None,
        }
    }

    /// Default class for a corpus category: findings default to embargoed
    /// (verified PoCs are crown jewels), everything else to internal.
    pub fn default_for_category(category: &str) -> Self {
        if category == "findings" {
            Sensitivity::Embargoed
        } else {
            Sensitivity::Internal
        }
    }

    /// True when an entry of this class needs the explicit `confirm` flag
    /// to leave a team scope. Only embargoed entries do.
    pub fn promotion_requires_confirm(self) -> bool {
        self == Sensitivity::Embargoed
    }

    /// Read the class from a page's frontmatter, defaulting by category.
    /// An invalid `sensitivity:` value fails loud rather than silently
    /// downgrading an embargoed entry to internal.
    pub fn from_frontmatter(
        fm: &crate::yaml::Mapping,
        category: &str,
    ) -> crate::error::Result<Self> {
        match crate::frontmatter::get_str(fm, "sensitivity") {
            Some(value) => Self::parse(&value).ok_or_else(|| {
                crate::error::Error::Store(format!(
                    "invalid sensitivity value in frontmatter: {value:?}"
                ))
            }),
            None => Ok(Self::default_for_category(category)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe() {
        assert_eq!(
            Sensitivity::default_for_category("findings"),
            Sensitivity::Embargoed
        );
        assert_eq!(
            Sensitivity::default_for_category("techniques"),
            Sensitivity::Internal
        );
        assert_eq!(
            Sensitivity::default_for_category("attacks"),
            Sensitivity::Internal
        );
        assert_eq!(
            Sensitivity::default_for_category("hypotheses"),
            Sensitivity::Internal
        );
        assert_eq!(
            Sensitivity::default_for_category("runs"),
            Sensitivity::Internal
        );
    }

    #[test]
    fn only_embargoed_needs_confirm() {
        assert!(Sensitivity::Embargoed.promotion_requires_confirm());
        assert!(!Sensitivity::Internal.promotion_requires_confirm());
        assert!(!Sensitivity::Open.promotion_requires_confirm());
    }
}
