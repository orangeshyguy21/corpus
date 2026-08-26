//! Markdown + YAML frontmatter helpers for persisted corpus data.
//!
//! Store pages are markdown with a leading `---`-fenced YAML block. These
//! helpers split and mutate that block WITHOUT reformatting the page body —
//! the store's "relocation, not reformat" rule is enforced here.

use crate::error::Error;
use crate::yaml::{self, Mapping, Value};

/// Split a wiki page into its frontmatter mapping and body.
///
/// A page with no frontmatter fence yields `(None, entire_text)`. A page
/// whose fence contains invalid YAML is an error — malformed frontmatter is
/// the kind of thing that silently breaks every downstream reader.
pub fn split(text: &str) -> Result<(Option<Mapping>, &str), Error> {
    let Some(after_open) = text.strip_prefix("---\n") else {
        return Ok((None, text));
    };
    let Some(rest) = after_open.find("\n---\n") else {
        return Ok((None, text));
    };
    let fm_text = &after_open[..rest];
    let body = &after_open[rest + "\n---\n".len()..];
    let mapping: Mapping = yaml::from_str(fm_text)?;
    Ok((Some(mapping), body))
}

/// Read a value from a frontmatter mapping.
pub fn get<'a>(fm: &'a Mapping, key: &str) -> Option<&'a Value> {
    fm.get(Value::String(key.to_string()))
}

/// Get a string value from a frontmatter mapping.
pub fn get_str(fm: &Mapping, key: &str) -> Option<String> {
    get(fm, key).and_then(|v| v.as_str()).map(str::to_string)
}

/// Insert `key: value` pairs into a page's frontmatter, preserving the rest
/// of the file byte-for-byte.
///
/// If the page has no frontmatter fence, one is created in front of the
/// original text. Adds are placed immediately after the opening fence, so
/// existing keys and the body are never rewritten. An addition whose key is
/// already present in the frontmatter is skipped — folding in a class an
/// entry already declares must not duplicate it.
pub fn insert_into_frontmatter(text: &str, additions: &[(&str, &str)]) -> Result<String, Error> {
    if text.is_empty() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cannot add frontmatter to an empty file",
        )));
    }
    if let Some(after_open) = text.strip_prefix("---\n") {
        match after_open.find("\n---\n") {
            // Existing fence: splice additions right after the opening fence.
            Some(pos) => {
                let fm_text = &after_open[..pos];
                let existing: Mapping = yaml::from_str(fm_text)?;
                let close_start = pos + "\n---\n".len();
                let mut out = String::with_capacity(text.len() + 64);
                out.push_str("---\n");
                for (key, value) in additions {
                    if existing.contains_key(Value::String(key.to_string())) {
                        continue; // never duplicate a declared key
                    }
                    out.push_str(key);
                    out.push_str(": ");
                    out.push_str(value);
                    out.push('\n');
                }
                out.push_str(&text["---\n".len()..close_start]);
                out.push_str(&text[close_start..]);
                Ok(out)
            }
            None => Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unterminated frontmatter fence",
            ))),
        }
    } else {
        // No frontmatter: prepend a fresh fence.
        let mut out = String::with_capacity(text.len() + 48);
        out.push_str("---\n");
        for (key, value) in additions {
            out.push_str(key);
            out.push_str(": ");
            out.push_str(value);
            out.push('\n');
        }
        out.push_str("---\n");
        out.push_str(text);
        Ok(out)
    }
}
