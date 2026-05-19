//! Stable artifact identifiers.
//!
//! Every node and edge in SpecSlice has a deterministic string ID built from
//! its kind, source path and stable key (e.g. a requirement ID, a class name,
//! a file path). The IDs are stable across reruns so that idempotent upsert is
//! straightforward.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactId(String);

impl ArtifactId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ArtifactId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for ArtifactId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// ID for a requirement node (`REQ-*`, `AC-*`, `ADR-*`).
pub fn requirement_id(raw: &str) -> ArtifactId {
    ArtifactId::new(format!("req::{}", raw.trim()))
}

/// ID for a markdown document section, addressed by file path and the slug of
/// its heading text.
pub fn doc_section_id(path: &str, slug: &str) -> ArtifactId {
    ArtifactId::new(format!("docsec::{path}#{slug}"))
}

/// ID for a source file artifact regardless of language.
pub fn file_id(path: &str) -> ArtifactId {
    ArtifactId::new(format!("file::{path}"))
}

/// ID for a Dart class symbol.
pub fn dart_class_id(path: &str, class: &str) -> ArtifactId {
    ArtifactId::new(format!("dart_class::{path}#{class}"))
}

/// ID for a Dart method symbol (instance method on a class).
pub fn dart_method_id(path: &str, class: &str, method: &str) -> ArtifactId {
    ArtifactId::new(format!("dart_method::{path}#{class}.{method}"))
}

/// ID for a top-level Dart function.
pub fn dart_function_id(path: &str, function: &str) -> ArtifactId {
    ArtifactId::new(format!("dart_fn::{path}#{function}"))
}

/// ID for a Dart constructor.
pub fn dart_constructor_id(path: &str, class: &str, ctor: &str) -> ArtifactId {
    let suffix = if ctor.is_empty() { "<default>" } else { ctor };
    ArtifactId::new(format!("dart_ctor::{path}#{class}.{suffix}"))
}

/// ID for a Dart `test(...)` call inside a test file. The slug is the kebab
/// version of the test description used in PRD examples.
pub fn dart_test_id(path: &str, slug: &str) -> ArtifactId {
    ArtifactId::new(format!("dart_test::{path}#{slug}"))
}

/// ID for a Dart `group(...)` block.
pub fn dart_group_id(path: &str, slug: &str) -> ArtifactId {
    ArtifactId::new(format!("dart_group::{path}#{slug}"))
}

/// Convert any user-visible string into a stable, ascii-only slug used inside
/// node IDs. Whitespace and non-alphanumerics collapse to `-`, leading and
/// trailing dashes are stripped, and the result is lowercased.
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_dash = true;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "section".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirement_id_prefixes_with_req() {
        assert_eq!(
            requirement_id("REQ-WATERMARK-001").as_str(),
            "req::REQ-WATERMARK-001"
        );
    }

    #[test]
    fn doc_section_id_includes_path_and_slug() {
        assert_eq!(
            doc_section_id("docs/watermark.md", "auto-watermark-placement").as_str(),
            "docsec::docs/watermark.md#auto-watermark-placement"
        );
    }

    #[test]
    fn file_id_prefixes_with_file() {
        assert_eq!(file_id("lib/main.dart").as_str(), "file::lib/main.dart");
    }

    #[test]
    fn dart_symbol_ids_include_class_and_member() {
        assert_eq!(
            dart_class_id("lib/a.dart", "Foo").as_str(),
            "dart_class::lib/a.dart#Foo"
        );
        assert_eq!(
            dart_method_id("lib/a.dart", "Foo", "bar").as_str(),
            "dart_method::lib/a.dart#Foo.bar"
        );
        assert_eq!(
            dart_function_id("lib/a.dart", "main").as_str(),
            "dart_fn::lib/a.dart#main"
        );
        assert_eq!(
            dart_constructor_id("lib/a.dart", "Foo", "named").as_str(),
            "dart_ctor::lib/a.dart#Foo.named"
        );
        assert_eq!(
            dart_constructor_id("lib/a.dart", "Foo", "").as_str(),
            "dart_ctor::lib/a.dart#Foo.<default>"
        );
    }

    #[test]
    fn dart_test_and_group_ids_use_kebab_slug() {
        assert_eq!(
            dart_test_id("test/a_test.dart", "places-watermark-outside-face-region").as_str(),
            "dart_test::test/a_test.dart#places-watermark-outside-face-region"
        );
        assert_eq!(
            dart_group_id("test/a_test.dart", "auto-placement").as_str(),
            "dart_group::test/a_test.dart#auto-placement"
        );
    }

    #[test]
    fn slugify_handles_unicode_punctuation_and_empty() {
        assert_eq!(
            slugify("Auto watermark placement"),
            "auto-watermark-placement"
        );
        assert_eq!(slugify("自动水印放置"), "section");
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify(""), "section");
        assert_eq!(slugify("---"), "section");
    }

    #[test]
    fn artifact_id_display_round_trip() {
        let id = ArtifactId::new("foo");
        assert_eq!(id.to_string(), "foo");
        assert_eq!(ArtifactId::from("foo"), id);
        assert_eq!(ArtifactId::from(String::from("foo")), id);
    }
}
