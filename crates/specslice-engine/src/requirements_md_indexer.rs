//! Markdown requirements indexer (P23.9).
//!
//! SpecSlice stays non-invasive: requirement↔code/doc/test relationships live
//! under `.specslice/requirements/*.md`, never in the target source. This is
//! the recommended, human-friendly successor to `.specslice/links.yaml`
//! (which keeps working — see [`crate::links_indexer`]).
//!
//! ## File format (Chinese-friendly, language-agnostic)
//!
//! ```markdown
//! # REQ-001 自动水印放置
//!
//! 一句话描述需求意图（可多行，作为 Requirement 节点的描述）。
//!
//! ## 文档
//! - docs/watermark.md#自动放置
//!
//! ## 实现
//! - lib/watermark.dart#WatermarkPlacer
//! - crates/foo/src/lib.rs#place
//!
//! ## 测试
//! - test/watermark_test.dart#places outside face
//! ```
//!
//! - One file may hold many requirements; each `# ` (H1) starts a new one.
//! - The first whitespace-delimited token of the H1 is the **id**; the rest is
//!   the **title** (any language).
//! - Section headers accept Chinese or English: 文档/docs, 实现/implementation,
//!   测试/test. Each is a markdown list of `path#fragment` references.
//! - Edges mirror the manifest indexer: `Documents`,
//!   `DeclaresImplementation`, `DeclaresVerification`, all pointing **into**
//!   the requirement so `slice` / `impact` / `checks` treat them identically.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::artifact_id::{doc_section_id, file_id, requirement_id, slugify};
use specslice_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use specslice_store::Store;

pub const REQUIREMENTS_MD_INDEXER_NAME: &str = "requirements_md";
/// Default directory (relative to the repo root) scanned for requirement docs.
pub const DEFAULT_REQUIREMENTS_DIR: &str = ".specslice/requirements";

#[derive(Debug, Clone)]
pub struct RequirementsMdIndexOptions {
    pub repo_root: PathBuf,
    /// Directory holding `*.md` requirement files (absolute, or relative to
    /// `repo_root`).
    pub requirements_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RequirementsMdIndexResult {
    pub files: usize,
    pub requirements: usize,
    pub documents: usize,
    pub implementations: usize,
    pub verifications: usize,
    pub edges: usize,
    /// References whose target could not be located in the current graph
    /// (still wired as a best-effort edge so `checks` can flag them).
    pub unresolved: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedRequirement {
    id: String,
    title: String,
    description: String,
    line: u32,
    docs: Vec<String>,
    implementations: Vec<String>,
    verifications: Vec<String>,
}

pub fn index_requirements_md(
    store: &mut Store,
    options: &RequirementsMdIndexOptions,
) -> Result<RequirementsMdIndexResult> {
    let dir = if options.requirements_dir.is_absolute() {
        options.requirements_dir.clone()
    } else {
        options.repo_root.join(&options.requirements_dir)
    };
    let mut result = RequirementsMdIndexResult::default();
    if !dir.is_dir() {
        return Ok(result);
    }

    // Snapshot existing nodes once for language-agnostic strict resolution.
    let all_nodes = store.list_all_nodes().context("listing nodes")?;

    // Deterministic file order.
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(&dir).sort_by_file_name() {
        let entry = entry.context("walking requirements dir")?;
        let path = entry.path();
        let is_md = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));
        // `README.md` is reserved for human documentation of the format (the
        // `init` scaffold), never parsed as requirements.
        let is_readme = path
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("README.md"));
        if entry.file_type().is_file() && is_md && !is_readme {
            files.push(path.to_path_buf());
        }
    }

    for file in files {
        let raw = std::fs::read_to_string(&file)
            .with_context(|| format!("reading requirement file {}", file.display()))?;
        let rel = file
            .strip_prefix(&options.repo_root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        result.files += 1;

        for req in parse_requirements(&raw) {
            let req_id = requirement_id(&req.id);
            let mut node = Node::new(req_id.clone(), NodeKind::Requirement);
            node.name = Some(if req.title.is_empty() {
                req.id.clone()
            } else {
                req.title.clone()
            });
            node.path = Some(rel.clone());
            node.start_line = Some(req.line);
            node.stable_key = Some(req.id.clone());
            node.source_file = Some(rel.clone());
            node.indexer = Some(REQUIREMENTS_MD_INDEXER_NAME.into());
            if !req.description.is_empty() {
                node.metadata_json =
                    Some(serde_json::json!({ "description": req.description }).to_string());
            }
            store.upsert_node(&node).context("upserting requirement")?;
            result.requirements += 1;

            for spec in &req.docs {
                let (id, resolved) = resolve_doc(&all_nodes, spec);
                upsert_edge(
                    store,
                    id,
                    req_id.clone(),
                    EdgeKind::Documents,
                    &rel,
                    &mut result,
                )?;
                result.documents += 1;
                if !resolved {
                    result.unresolved += 1;
                }
            }
            for spec in &req.implementations {
                let (id, resolved) = resolve_code(&all_nodes, spec, code_impl_kind);
                upsert_edge(
                    store,
                    id,
                    req_id.clone(),
                    EdgeKind::DeclaresImplementation,
                    &rel,
                    &mut result,
                )?;
                result.implementations += 1;
                if !resolved {
                    result.unresolved += 1;
                }
            }
            for spec in &req.verifications {
                let (id, resolved) = resolve_code(&all_nodes, spec, NodeKind::is_test);
                upsert_edge(
                    store,
                    id,
                    req_id.clone(),
                    EdgeKind::DeclaresVerification,
                    &rel,
                    &mut result,
                )?;
                result.verifications += 1;
                if !resolved {
                    result.unresolved += 1;
                }
            }
        }
    }
    Ok(result)
}

fn upsert_edge(
    store: &mut Store,
    from_id: ArtifactId,
    to_id: ArtifactId,
    kind: EdgeKind,
    source_file: &str,
    result: &mut RequirementsMdIndexResult,
) -> Result<()> {
    let mut edge = EdgeAssertion::declared(from_id, to_id, kind, EdgeSource::ExternalManifest);
    edge.indexer = Some(REQUIREMENTS_MD_INDEXER_NAME.into());
    edge.source_file = Some(source_file.to_string());
    store
        .upsert_edge(&edge)
        .context("upserting requirement edge")?;
    result.edges += 1;
    Ok(())
}

/// Implementation refs target any user-defined type container or callable.
fn code_impl_kind(kind: NodeKind) -> bool {
    kind.is_type_container() || kind.is_callable()
}

fn is_doc_section(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::DocSection)
}

/// Resolve a doc reference; `bool` is whether a real node was found.
fn resolve_doc(all: &[Node], spec: &str) -> (ArtifactId, bool) {
    let (path, fragment) = split_ref(spec);
    if let Some(frag) = fragment {
        if let Some(id) = find_node(all, is_doc_section, path, frag) {
            return (id, true);
        }
        let id = doc_section_id(path, &slugify(frag));
        let exists = all.iter().any(|n| n.id == id);
        (id, exists)
    } else {
        let id = file_id(path);
        let exists = all.iter().any(|n| n.id == id);
        (id, exists)
    }
}

/// Resolve a code reference (implementation or test) language-agnostically.
fn resolve_code(all: &[Node], spec: &str, pred: fn(NodeKind) -> bool) -> (ArtifactId, bool) {
    let (path, fragment) = split_ref(spec);
    match fragment {
        Some(frag) => {
            if let Some(id) = find_node(all, pred, path, frag) {
                (id, true)
            } else {
                (file_id(path), false)
            }
        }
        None => {
            let id = file_id(path);
            let exists = all.iter().any(|n| n.id == id);
            (id, exists)
        }
    }
}

/// Find a node under `path` whose name matches `fragment` (exact, stable key,
/// slug, or the last dotted segment for `Type.member` style references),
/// restricted to kinds passing `pred`.
fn find_node(
    all: &[Node],
    pred: fn(NodeKind) -> bool,
    path: &str,
    fragment: &str,
) -> Option<ArtifactId> {
    let last = fragment.rsplit('.').next().unwrap_or(fragment);
    let frag_slug = slugify(fragment);
    let last_slug = slugify(last);
    for n in all {
        if n.path.as_deref() != Some(path) || !pred(n.kind) {
            continue;
        }
        let name = n.name.as_deref();
        let stable = n.stable_key.as_deref();
        let matches = name == Some(fragment)
            || name == Some(last)
            || stable == Some(fragment)
            || stable == Some(last)
            || name.map(slugify).as_deref() == Some(frag_slug.as_str())
            || name.map(slugify).as_deref() == Some(last_slug.as_str());
        if matches {
            return Some(n.id.clone());
        }
    }
    None
}

fn split_ref(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('#') {
        Some((path, fragment)) if !fragment.trim().is_empty() => {
            (path.trim(), Some(fragment.trim()))
        }
        Some((path, _)) => (path.trim(), None),
        None => (spec.trim(), None),
    }
}

/// Parse one markdown file into its requirement blocks.
fn parse_requirements(content: &str) -> Vec<ParsedRequirement> {
    let mut out: Vec<ParsedRequirement> = Vec::new();
    let mut cur: Option<ParsedRequirement> = None;
    let mut section = Section::Body;
    let mut in_fence = false;

    for (idx, line) in content.lines().enumerate() {
        // Fenced code blocks are inert: `#`/`-` lines inside ``` … ``` (or ~~~)
        // are literal content, never requirement headings or references. This
        // lets a requirements file embed sample snippets safely.
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(rest) = h1_text(line) {
            if let Some(done) = cur.take() {
                out.push(done);
            }
            let (id, title) = split_h1(rest);
            cur = Some(ParsedRequirement {
                id,
                title,
                line: u32::try_from(idx.saturating_add(1)).unwrap_or(u32::MAX),
                ..Default::default()
            });
            section = Section::Body;
            continue;
        }
        if let Some(h2) = line.strip_prefix("## ") {
            section = Section::classify(h2.trim());
            continue;
        }
        let Some(req) = cur.as_mut() else { continue };
        match section {
            Section::Body => {
                let t = line.trim();
                if !t.is_empty() {
                    if !req.description.is_empty() {
                        req.description.push(' ');
                    }
                    req.description.push_str(t);
                }
            }
            Section::Docs | Section::Impl | Section::Test => {
                if let Some(reference) = parse_list_item(line) {
                    match section {
                        Section::Docs => req.docs.push(reference),
                        Section::Impl => req.implementations.push(reference),
                        Section::Test => req.verifications.push(reference),
                        Section::Body => unreachable!(),
                    }
                }
            }
        }
    }
    if let Some(done) = cur.take() {
        out.push(done);
    }
    out
}

/// `"# Foo"` → `Some("Foo")`; `"## Foo"` / non-headings → `None`.
fn h1_text(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("# ")?;
    Some(rest.trim())
}

fn split_h1(text: &str) -> (String, String) {
    let text = text.trim();
    match text.split_once(char::is_whitespace) {
        Some((id, title)) => {
            let title = title
                .trim()
                .trim_start_matches(['—', '-', ':', '：', '–'])
                .trim();
            (id.to_string(), title.to_string())
        }
        None => (text.to_string(), String::new()),
    }
}

/// Which list a `## ` heading introduces. Accepts Chinese or English labels.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    Body,
    Docs,
    Impl,
    Test,
}

impl Section {
    fn classify(name: &str) -> Section {
        let lower = name.to_ascii_lowercase();
        if name.contains("文档") || lower.starts_with("doc") {
            Section::Docs
        } else if name.contains("实现")
            || name.contains("代码")
            || lower.contains("impl")
            || lower.contains("code")
        {
            Section::Impl
        } else if name.contains("测试")
            || name.contains("验证")
            || lower.contains("test")
            || lower.contains("verif")
        {
            Section::Test
        } else {
            Section::Body
        }
    }
}

fn parse_list_item(line: &str) -> Option<String> {
    let t = line.trim_start();
    let body = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))?;
    clean_reference(body)
}

/// Extract a `path#fragment` reference from a list item body, tolerating
/// inline code backticks and `[text](target)` markdown links.
fn clean_reference(body: &str) -> Option<String> {
    let s = body.trim();
    // Markdown link: take the target inside (...).
    if let Some(open) = s.find("](") {
        if let Some(close_rel) = s[open + 2..].find(')') {
            let target = &s[open + 2..open + 2 + close_rel];
            return normalise_reference(target);
        }
    }
    normalise_reference(s)
}

fn normalise_reference(s: &str) -> Option<String> {
    let s = s.trim().trim_matches('`').trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_requirements_with_chinese_sections() {
        let md = "# REQ-1 第一个需求\n\
            意图描述第一行。\n\
            第二行描述。\n\n\
            ## 文档\n- docs/a.md#概述\n\n\
            ## 实现\n- lib/a.dart#Foo\n- `lib/b.dart#Bar`\n\n\
            ## 测试\n- test/a_test.dart#用例一\n\n\
            # REQ-2 第二个需求\n\n\
            ## implementation\n- crates/x/src/lib.rs#run\n";
        let reqs = parse_requirements(md);
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].id, "REQ-1");
        assert_eq!(reqs[0].title, "第一个需求");
        assert_eq!(reqs[0].description, "意图描述第一行。 第二行描述。");
        assert_eq!(reqs[0].docs, vec!["docs/a.md#概述"]);
        assert_eq!(
            reqs[0].implementations,
            vec!["lib/a.dart#Foo", "lib/b.dart#Bar"]
        );
        assert_eq!(reqs[0].verifications, vec!["test/a_test.dart#用例一"]);
        assert_eq!(reqs[1].id, "REQ-2");
        assert_eq!(reqs[1].implementations, vec!["crates/x/src/lib.rs#run"]);
    }

    #[test]
    fn h1_parsing_ignores_h2_and_strips_title_separators() {
        assert_eq!(h1_text("## Not H1"), None);
        assert_eq!(h1_text("#NoSpace"), None);
        let (id, title) = split_h1("P23.9 — Markdown 需求");
        assert_eq!(id, "P23.9");
        assert_eq!(title, "Markdown 需求");
        let (id, title) = split_h1("LONE");
        assert_eq!(id, "LONE");
        assert_eq!(title, "");
    }

    #[test]
    fn section_classifier_accepts_both_languages() {
        assert!(matches!(Section::classify("文档"), Section::Docs));
        assert!(matches!(Section::classify("Docs"), Section::Docs));
        assert!(matches!(Section::classify("实现"), Section::Impl));
        assert!(matches!(Section::classify("Implementation"), Section::Impl));
        assert!(matches!(Section::classify("测试"), Section::Test));
        assert!(matches!(Section::classify("Verification"), Section::Test));
        assert!(matches!(Section::classify("随便写的"), Section::Body));
    }

    #[test]
    fn reference_cleaning_handles_backticks_and_markdown_links() {
        assert_eq!(
            parse_list_item("- `lib/a.dart#Foo`").as_deref(),
            Some("lib/a.dart#Foo")
        );
        assert_eq!(
            parse_list_item("  * [Foo 类](lib/a.dart#Foo)").as_deref(),
            Some("lib/a.dart#Foo")
        );
        assert_eq!(parse_list_item("not a list item"), None);
        assert_eq!(parse_list_item("- "), None);
    }

    #[test]
    fn fenced_code_blocks_are_not_parsed_as_requirements() {
        let md = "# REQ-REAL 真需求\n\n\
            ## 实现\n- lib/a.dart#Foo\n\n\
            说明，示例如下：\n\n\
            ```markdown\n\
            # REQ-FAKE 不应被解析\n\
            ## 实现\n- lib/fake.dart#Nope\n\
            ```\n";
        let reqs = parse_requirements(md);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].id, "REQ-REAL");
        assert_eq!(reqs[0].implementations, vec!["lib/a.dart#Foo"]);
    }

    #[test]
    fn split_ref_separates_path_and_fragment() {
        assert_eq!(split_ref("lib/a.dart#Foo"), ("lib/a.dart", Some("Foo")));
        assert_eq!(split_ref("lib/a.dart#"), ("lib/a.dart", None));
        assert_eq!(split_ref("lib/a.dart"), ("lib/a.dart", None));
    }
}
