//! Stable artifact identifiers.
//!
//! Every node and edge in GroundGraph has a deterministic string ID built from
//! its kind, source path and stable key (e.g. a requirement ID, a class name,
//! a file path). The IDs are stable across reruns so that idempotent upsert is
//! straightforward.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
///
/// Inputs with no ASCII alphanumerics at all (pure-CJK headings — the
/// dominant case for Chinese docs) fall back to a content hash instead of a
/// fixed word, so two different headings can never collide on one id
/// (issues2.md #53).
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_dash = true;
    let mut has_non_ascii = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else {
            // Non-ASCII chars (emoji, CJK) are dropped from the slug entirely.
            if !ch.is_ascii() {
                has_non_ascii = true;
            }
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        format!("s{:016x}", fnv1a64(text.as_bytes()))
    } else if has_non_ascii {
        // The slug dropped one or more non-ASCII chars, so two headings that
        // differ only in those chars (`Rocket 🚀` vs `Rocket 🚀🚀`) would
        // collide and UPSERT over each other. Append a content hash to keep
        // them distinct — generalises issues2.md #53 (pure-CJK) to mixed
        // ASCII+non-ASCII (#203).
        format!("{out}-{:016x}", fnv1a64(text.as_bytes()))
    } else {
        out
    }
}

/// FNV-1a 64-bit — tiny, dependency-free, deterministic. Only used for the
/// slug fallback above; not a cryptographic hash and does not need to be.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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
        assert_eq!(slugify("Hello, World!"), "hello-world");
    }

    /// Two different all-CJK headings must NOT collapse onto one slug —
    /// the old fixed `"section"` fallback merged every Chinese doc
    /// section in a file into a single node id (issues2.md #53).
    #[test]
    fn slugify_keeps_distinct_cjk_headings_distinct_and_deterministic() {
        let a = slugify("自动水印放置");
        let b = slugify("用户登录鉴权");
        assert_ne!(a, b, "distinct CJK headings must produce distinct slugs");
        assert_eq!(a, slugify("自动水印放置"), "slug must be deterministic");
        assert!(
            a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "slug must stay ascii-safe: {a}"
        );
        // Truly empty inputs still need *some* stable non-empty slug.
        assert!(!slugify("").is_empty());
        assert_ne!(slugify("---"), slugify("自动水印放置"));
    }

    /// Headings that share an ASCII prefix but differ only in dropped
    /// non-ASCII chars (emoji/CJK) must NOT collide — otherwise two doc
    /// sections claim one id and UPSERT over each other (#203).
    #[test]
    fn slugify_disambiguates_mixed_ascii_and_non_ascii_collisions() {
        let a = slugify("Rocket 🚀");
        let b = slugify("Rocket 🚀🚀");
        assert_ne!(a, b, "emoji-count difference must not collapse to one id");
        assert!(a.starts_with("rocket-"), "keeps readable ascii stem: {a}");
        assert!(b.starts_with("rocket-"), "keeps readable ascii stem: {b}");
        assert_eq!(a, slugify("Rocket 🚀"), "must stay deterministic");
        // A mixed CJK+ASCII heading is also disambiguated, not bare.
        assert_ne!(slugify("登录 login"), slugify("登出 login"));
        // Pure-ASCII slugs are unchanged (no hash suffix).
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn artifact_id_display_round_trip() {
        let id = ArtifactId::new("foo");
        assert_eq!(id.to_string(), "foo");
        assert_eq!(ArtifactId::from("foo"), id);
        assert_eq!(ArtifactId::from(String::from("foo")), id);
    }
}
