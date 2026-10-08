//! F-044 test support: read a content field's literal text off the *shipped*
//! RON, so a mutation anchor (or an expected number) tracks a re-tune instead
//! of pinning the balance value it happens to have today.
//!
//! A test that mutates content by textual substitution needs an anchor that
//! exists in the file. Writing the shipped value into the test (`"damage_mult:
//! 1.3,"`) turns every balance change into a test failure that says nothing
//! about the code; reading the anchor here keeps the mutation and drops the pin.
//!
//! Include with `#[path = "support/ron_field.rs"] mod ron_field;`.
#![allow(dead_code)]

use std::path::PathBuf;

/// The shipped text of `assets/data/<file>`.
pub fn shipped(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/data")
        .join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// One field occurrence in a RON text: `from` is a substring unique enough to
/// substitute on, and `value` the field's literal value as written.
#[derive(Debug, Clone)]
pub struct Field {
    /// The text a substitution replaces (ends with the field's `,` when it has one).
    pub from: String,
    /// The literal value, e.g. `1.3` or `90` or `"ravager"`.
    pub value: String,
    head: String,
    tail: String,
    /// Offset of the key within `head` (everything before it is context).
    key_at: usize,
}

impl Field {
    /// `from` with the value replaced by `new`.
    pub fn with(&self, new: impl std::fmt::Display) -> String {
        format!("{}{new}{}", self.head, self.tail)
    }

    /// `from` with the whole `key: value,` removed (the prefix before the key
    /// is kept, so only the field disappears).
    pub fn removed(&self) -> String {
        self.head[..self.key_at].to_string()
    }

    /// The value parsed as a number (panics with the field text if it is not).
    pub fn num<T: std::str::FromStr>(&self) -> T {
        self.value
            .parse()
            .unwrap_or_else(|_| panic!("`{}` is not a number", self.from))
    }
}

/// Byte offset of the first `key: ` in `text[start..]` that is a whole field
/// name (not the tail of a longer one, e.g. `hp_per_defense` inside
/// `building_hp_per_defense`), skipping `//` comment text.
fn find_key(text: &str, start: usize, key: &str) -> usize {
    let pat = format!("{key}: ");
    let mut from = start;
    while let Some(rel) = text[from..].find(&pat) {
        let at = from + rel;
        let before = text[..at].chars().next_back();
        let boundary = !matches!(before, Some(c) if c.is_ascii_alphanumeric() || c == '_');
        let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
        let in_comment = text[line_start..at].contains("//");
        if boundary && !in_comment {
            return at;
        }
        from = at + pat.len();
    }
    panic!("no `{key}:` field found in the shipped RON");
}

fn build(text: &str, span_start: usize, key_at: usize, key: &str) -> Field {
    let value_at = key_at + key.len() + 2;
    let rest = &text[value_at..];
    let end = rest
        .find([',', ')', '\n'])
        .unwrap_or_else(|| panic!("unterminated `{key}` field"));
    let value = rest[..end].trim_end().to_string();
    let comma = if rest[end..].starts_with(',') { "," } else { "" };
    let head = text[span_start..value_at].to_string();
    let tail = format!("{}{comma}", &rest[value.len()..end]);
    Field {
        from: format!("{head}{value}{tail}"),
        value,
        head,
        tail,
        key_at: key_at - span_start,
    }
}

/// The first whole-name `key:` field in `text`. `from` includes the character
/// before the key, so it cannot match inside a longer field name.
pub fn field(text: &str, key: &str) -> Field {
    let at = find_key(text, 0, key);
    let start = text[..at]
        .char_indices()
        .next_back()
        .map_or(0, |(i, _)| i);
    build(text, start, at, key)
}

/// The first `key:` field after the anchor `after` (e.g. `id: "bulwark"`).
/// `from` spans from the anchor to the field, so it is unique to that entry.
pub fn field_after(text: &str, after: &str, key: &str) -> Field {
    let anchor = text
        .find(after)
        .unwrap_or_else(|| panic!("anchor `{after}` missing from the shipped RON"));
    let at = find_key(text, anchor + after.len(), key);
    build(text, anchor, at, key)
}
