//! Markdown document indexer.
//!
//! MVP-1 scope (PRD §3.1, implementation plan §MVP-1):
//! - Walk every `*.md` and `*.mdx` file under the configured docs roots.
//! - Treat Markdown as physical evidence only: file, heading sections, line
//!   ranges and content hash.
//! - Never infer business requirements from frontmatter or heading text. AI
//!   may later propose business logic candidates from these document facts,
//!   and only accepted external graph data creates `Requirement` nodes.
//! - Emit `File --contains--> DocSection` (Fact).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specslice_core::{
    artifact_id::{doc_section_id, file_id, slugify},
    EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind,
};
use specslice_store::Store;

pub const DOCS_INDEXER_NAME: &str = "docs_markdown";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DocsIndexResult {
    pub files: usize,
    pub requirements: usize,
    pub doc_sections: usize,
    pub edges: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Heading {
    level: u8,
    text: String,
    line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedDoc {
    headings: Vec<Heading>,
    total_lines: u32,
    content_hash: String,
}

#[derive(Debug, Clone)]
pub struct DocsIndexOptions {
    pub repo_root: PathBuf,
    pub doc_roots: Vec<PathBuf>,
    pub include_globs: Vec<String>,
}

/// Walk all configured doc roots and merge results into the given store.
pub fn index_docs(store: &mut Store, options: &DocsIndexOptions) -> Result<DocsIndexResult> {
    let mut result = DocsIndexResult::default();
    let mut visited = Vec::new();
    for root in &options.doc_roots {
        let abs_root = options.repo_root.join(root);
        if !abs_root.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&abs_root)
            .into_iter()
            .filter_entry(|e| !is_pruned_dir(e))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let is_doc = matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("md") | Some("mdx") | Some("markdown") | Some("rst")
            );
            if !is_doc {
                continue;
            }
            let rel = path
                .strip_prefix(&options.repo_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if !matches_include_globs(&options.include_globs, &rel)? {
                continue;
            }
            if visited.iter().any(|v| v == &rel) {
                continue;
            }
            visited.push(rel.clone());
            index_one_file(store, &rel, path, &mut result)
                .with_context(|| format!("indexing markdown file {rel}"))?;
        }
    }

    // Well-known root documents. The repository README is the primary (often
    // the only) document of a project — tokio's main doc *is* its README —
    // but `doc_roots` lists directories, and making users add `.` would walk
    // the whole tree. Pick up the conventional root files unconditionally;
    // `visited` already dedupes when a doc root covered them.
    for entry in std::fs::read_dir(&options.repo_root).into_iter().flatten() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_well_known_root_doc(name) {
            continue;
        }
        let rel = name.to_string();
        if visited.iter().any(|v| v == &rel) {
            continue;
        }
        visited.push(rel.clone());
        index_one_file(store, &rel, &path, &mut result)
            .with_context(|| format!("indexing root document {rel}"))?;
    }
    Ok(result)
}

/// Conventional top-level documents that double as a project's de-facto
/// specification: README, ARCHITECTURE, DESIGN, CONTRIBUTING. Extension must
/// be a known prose format — code or data files sharing the stem stay out.
fn is_well_known_root_doc(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    let Some((stem, ext)) = lower.rsplit_once('.') else {
        return false;
    };
    matches!(ext, "md" | "mdx" | "markdown" | "rst")
        && ["readme", "architecture", "design", "contributing"]
            .iter()
            .any(|known| stem == *known || stem.starts_with(&format!("{known}.")))
}

/// Directories the docs walk must never descend into. Mirrors the tree-sitter
/// drivers' `skip_dirs`: vendored dependencies, VCS, build output and our own
/// `.specslice/` workspace would otherwise flood the graph with thousands of
/// third-party `README.md` sections (real dogfood bug: a checked-in
/// `node_modules` under a doc root produced 1.5k phantom doc sections).
fn is_pruned_dir(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    // A nested SpecSlice workspace (a sub-directory, depth > 0, holding its own
    // `.specslice.yaml`) is a separate project — vendored reference repos carry
    // their own config — indexed by its own `index`, never folded into the
    // parent doc graph. The doc root itself (depth 0) is exempt.
    if entry.depth() > 0
        && entry
            .path()
            .join(crate::config::DEFAULT_CONFIG_FILE_NAME)
            .is_file()
    {
        return true;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    matches!(
        name,
        "node_modules"
            | ".git"
            | ".hg"
            | ".svn"
            | "target"
            | "build"
            | "dist"
            | "out"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".next"
            | ".svelte-kit"
            | ".specslice"
            | "vendor"
            | ".idea"
            | ".tox"
    )
}

fn matches_include_globs(patterns: &[String], rel_path: &str) -> Result<bool> {
    if patterns.is_empty() {
        return Ok(true);
    }
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            globset::Glob::new(pattern)
                .with_context(|| format!("invalid docs.include glob `{pattern}`"))?,
        );
    }
    let set = builder
        .build()
        .context("building docs.include glob matcher")?;
    Ok(set.is_match(rel_path))
}

fn index_one_file(
    store: &mut Store,
    rel_path: &str,
    abs_path: &Path,
    result: &mut DocsIndexResult,
) -> Result<()> {
    let raw = std::fs::read_to_string(abs_path)
        .with_context(|| format!("reading {}", abs_path.display()))?;
    let is_rst = rel_path
        .rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("rst"));
    let parsed = if is_rst {
        parse_rst(&raw)
    } else {
        parse_markdown(&raw)
    };

    let mut file_node = Node::new(file_id(rel_path), NodeKind::File);
    file_node.path = Some(rel_path.to_string());
    file_node.name = abs_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned());
    file_node.start_line = Some(1);
    file_node.end_line = Some(parsed.total_lines);
    file_node.content_hash = Some(parsed.content_hash.clone());
    file_node.indexer = Some(DOCS_INDEXER_NAME.into());
    store.upsert_node(&file_node)?;
    result.files += 1;

    // Build doc sections from headings. A section runs from its heading line
    // until the next heading of equal or lower level (smaller `level` value
    // means a higher heading), or EOF.
    for (idx, heading) in parsed.headings.iter().enumerate() {
        let end_line = parsed
            .headings
            .iter()
            .skip(idx + 1)
            .find(|h| h.level <= heading.level)
            .map(|h| h.line.saturating_sub(1))
            .unwrap_or(parsed.total_lines);
        let slug = slugify(&heading.text);
        let section_id = doc_section_id(rel_path, &slug);
        let mut node = Node::new(section_id.clone(), NodeKind::DocSection);
        node.path = Some(rel_path.to_string());
        node.name = Some(heading.text.clone());
        node.start_line = Some(heading.line);
        node.end_line = Some(end_line);
        node.indexer = Some(DOCS_INDEXER_NAME.into());
        store.upsert_node(&node)?;
        result.doc_sections += 1;

        let contains_edge = make_indexed_edge(EdgeAssertion::fact(
            file_node.id.clone(),
            section_id.clone(),
            EdgeKind::Contains,
            EdgeSource::Markdown,
        ));
        store.upsert_edge(&contains_edge)?;
        result.edges += 1;
    }

    Ok(())
}

fn make_indexed_edge(mut edge: EdgeAssertion) -> EdgeAssertion {
    edge.indexer = Some(DOCS_INDEXER_NAME.into());
    edge
}

fn parse_markdown(raw: &str) -> ParsedDoc {
    let mut total_lines = 0u32;
    let mut headings = Vec::new();
    let mut content_hasher = Sha256::new();

    // Treat frontmatter as a physical prelude only. We skip it so YAML keys do
    // not become headings, but we never parse it into business semantics.
    let mut consumed_lines = 0usize;
    if raw.starts_with("---\n") || raw.starts_with("---\r\n") {
        let mut closed = false;
        for (idx, line) in raw.lines().enumerate().skip(1) {
            consumed_lines = idx + 1;
            if line.trim_end() == "---" {
                closed = true;
                break;
            }
        }
        if !closed {
            consumed_lines = 0;
        }
    }

    for (idx, line) in raw.lines().enumerate() {
        total_lines = u32::try_from(idx.saturating_add(1)).unwrap_or(u32::MAX);
        content_hasher.update(line.as_bytes());
        content_hasher.update(b"\n");
        if idx < consumed_lines {
            continue;
        }
        let trimmed = line.trim_start();
        if let Some((level, text)) = parse_heading(trimmed) {
            headings.push(Heading {
                level,
                text: text.to_string(),
                line: total_lines,
            });
            continue;
        }
    }
    if total_lines == 0 {
        total_lines = 1;
    }
    let content_hash = format!("{:x}", content_hasher.finalize());

    ParsedDoc {
        headings,
        total_lines,
        content_hash,
    }
}

/// Parse reStructuredText section titles: a text line followed by an
/// adornment line of one repeated punctuation character at least as long as
/// the text. Levels follow docutils semantics — assigned by order of first
/// appearance of each adornment character, not by a fixed table (rst has no
/// canonical mapping; `=`/`-`/`~` orders vary by project). A lone adornment
/// line after a blank line is a transition, not a title.
fn parse_rst(raw: &str) -> ParsedDoc {
    let mut total_lines = 0u32;
    let mut headings = Vec::new();
    let mut content_hasher = Sha256::new();

    let lines: Vec<&str> = raw.lines().collect();
    let mut adornment_levels: Vec<char> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        total_lines = u32::try_from(idx.saturating_add(1)).unwrap_or(u32::MAX);
        content_hasher.update(line.as_bytes());
        content_hasher.update(b"\n");

        let Some(adorn) = rst_adornment_char(line) else {
            continue;
        };
        // The line above must be a plausible title: non-empty, not itself an
        // adornment line (that would make *this* line part of an
        // overline/underline pair handled when we saw the overline), and not
        // longer than the adornment.
        let Some(title_idx) = idx.checked_sub(1) else {
            continue;
        };
        let title = lines[title_idx].trim();
        if title.is_empty() || rst_adornment_char(lines[title_idx]).is_some() {
            continue;
        }
        if title.chars().count() > line.trim_end().chars().count() {
            continue;
        }
        // Overline+underline style (`=== / Title / ===`) records once: the
        // overline is skipped above (its "title" is the adornment line), and
        // the underline lands here with the real text.
        let level_idx = adornment_levels
            .iter()
            .position(|&c| c == adorn)
            .unwrap_or_else(|| {
                adornment_levels.push(adorn);
                adornment_levels.len() - 1
            });
        headings.push(Heading {
            level: u8::try_from(level_idx + 1).unwrap_or(6).min(6),
            text: title.to_string(),
            line: u32::try_from(title_idx + 1).unwrap_or(u32::MAX),
        });
    }
    if total_lines == 0 {
        total_lines = 1;
    }
    ParsedDoc {
        headings,
        total_lines,
        content_hash: format!("{:x}", content_hasher.finalize()),
    }
}

/// `Some(c)` when the whole line is 3+ repetitions of one rst punctuation
/// character (the docutils adornment set), `None` otherwise.
fn rst_adornment_char(line: &str) -> Option<char> {
    let t = line.trim_end();
    let mut chars = t.chars();
    let first = chars.next()?;
    if !r##"!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~"##.contains(first) {
        return None;
    }
    if t.chars().count() < 3 {
        return None;
    }
    chars.all(|c| c == first).then_some(first)
}

fn parse_heading(line: &str) -> Option<(u8, &str)> {
    if !line.starts_with('#') {
        return None;
    }
    let mut level = 0u8;
    let bytes = line.as_bytes();
    for &b in bytes {
        if b == b'#' && level < 6 {
            level += 1;
        } else {
            break;
        }
    }
    if level == 0 {
        return None;
    }
    let rest = &line[level as usize..];
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((level, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_heading_levels_and_text() {
        assert_eq!(parse_heading("# Hello"), Some((1, "Hello")));
        assert_eq!(parse_heading("## World"), Some((2, "World")));
        assert_eq!(parse_heading("###### Six"), Some((6, "Six")));
        assert_eq!(parse_heading("####### Seven"), Some((6, "# Seven")));
        assert_eq!(parse_heading("nope"), None);
        assert_eq!(parse_heading("#"), None);
        assert_eq!(parse_heading("#    "), None);
    }

    #[test]
    fn parse_markdown_skips_frontmatter_without_semantic_parsing() {
        let src = "---\nid: REQ-1\ntype: requirement\ntitle: T\n---\n\n# Top\n\nBody\n\n## Details\n\nMore text.\n";
        let parsed = parse_markdown(src);
        assert_eq!(parsed.headings.len(), 2);
        assert_eq!(parsed.headings[0].level, 1);
        assert_eq!(parsed.headings[0].text, "Top");
        assert_eq!(parsed.headings[1].text, "Details");
    }

    #[test]
    fn parse_markdown_handles_no_frontmatter() {
        let parsed = parse_markdown("# Hello\n\nBody\n");
        assert_eq!(parsed.headings.len(), 1);
    }

    #[test]
    fn parse_markdown_handles_unterminated_frontmatter() {
        let parsed = parse_markdown("---\nid: REQ-1\n# Hello\n");
        // The frontmatter is unterminated → must fall back gracefully.
        assert_eq!(parsed.headings.len(), 1);
        assert_eq!(parsed.headings[0].text, "Hello");
    }

    /// Dogfood fix: a checked-in `node_modules` (or `.git`/`target`/…) under a
    /// doc root must not flood the graph with vendored `README.md` sections.
    #[test]
    fn docs_walk_prunes_vendor_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("docs/node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join("docs/sub")).unwrap();
        std::fs::write(root.join("docs/guide.md"), "# Guide\n\nbody\n").unwrap();
        std::fs::write(root.join("docs/sub/deep.md"), "# Deep\n\nbody\n").unwrap();
        std::fs::write(
            root.join("docs/node_modules/pkg/README.md"),
            "# Vendor\n\nnoise\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let opts = DocsIndexOptions {
            repo_root: root.to_path_buf(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: vec![],
        };
        let result = index_docs(&mut store, &opts).unwrap();
        assert_eq!(
            result.files, 2,
            "only docs/guide.md + docs/sub/deep.md, not node_modules; got {result:?}"
        );
        let nodes = store.list_all_nodes().unwrap();
        assert!(nodes
            .iter()
            .any(|n| n.id.to_string() == "file::docs/guide.md"));
        assert!(
            nodes
                .iter()
                .all(|n| !n.id.to_string().contains("node_modules")),
            "vendored node_modules markdown must be pruned"
        );
    }

    /// A vendored reference repo under a doc root carries its *own*
    /// `.specslice.yaml`; it is a separate workspace, indexed by its own
    /// `index`, and must not flood the parent graph with its README sections
    /// (real dogfood bug: tailorx bundling the Java `platform` + `bp-web` under
    /// `docs/references/source-repos/` leaked their READMEs into the doc graph).
    #[test]
    fn docs_walk_prunes_nested_specslice_workspaces() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vendored = root.join("docs/references/vendored");
        std::fs::create_dir_all(&vendored).unwrap();
        std::fs::write(root.join("docs/guide.md"), "# Guide\n\nbody\n").unwrap();
        std::fs::write(vendored.join(".specslice.yaml"), "repo:\n  root: .\n").unwrap();
        std::fs::write(vendored.join("README.md"), "# Vendor\n\nnoise\n").unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let opts = DocsIndexOptions {
            repo_root: root.to_path_buf(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: vec![],
        };
        let result = index_docs(&mut store, &opts).unwrap();
        assert_eq!(
            result.files, 1,
            "only docs/guide.md, not the nested workspace README; got {result:?}"
        );
        let nodes = store.list_all_nodes().unwrap();
        assert!(
            nodes.iter().all(|n| !n.id.to_string().contains("vendored")),
            "nested-workspace markdown must be pruned"
        );
    }

    /// reStructuredText is the documentation lingua franca of the Python
    /// ecosystem (flask, django, numpy, requests all ship `docs/*.rst`).
    /// Underlined titles must become DocSections exactly like `#` headings,
    /// with levels assigned by order of first adornment appearance (docutils
    /// semantics), and transitions (`----` after a blank line) must not.
    #[test]
    fn rst_underline_headings_become_doc_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(
            root.join("docs/blueprints.rst"),
            "Modular Applications with Blueprints\n\
             ====================================\n\
             \n\
             intro text\n\
             \n\
             Why Blueprints?\n\
             ---------------\n\
             \n\
             body\n\
             \n\
             ----\n\
             \n\
             My First Blueprint\n\
             ------------------\n\
             \n\
             more body\n",
        )
        .unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let opts = DocsIndexOptions {
            repo_root: root.to_path_buf(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: vec!["**/*.rst".into()],
        };
        let result = index_docs(&mut store, &opts).unwrap();
        assert_eq!(result.files, 1);
        assert_eq!(
            result.doc_sections, 3,
            "three underlined titles, the lone `----` transition is not one; got {result:?}"
        );
        let nodes = store.list_all_nodes().unwrap();
        let top = nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("Modular Applications with Blueprints"))
            .expect("page title section");
        assert_eq!(top.start_line, Some(1));
        assert!(
            nodes
                .iter()
                .any(|n| n.name.as_deref() == Some("Why Blueprints?")),
            "sub-section title must be a DocSection"
        );
    }

    /// The repository root README (and ARCHITECTURE/DESIGN/CONTRIBUTING) is
    /// the primary document of most projects — tokio's main doc *is* its
    /// README — yet `doc_roots` only lists directories like `docs/`. The
    /// indexer must always pick up these well-known root files, without
    /// configuration and without double-indexing when a root already covers
    /// them.
    #[test]
    fn repo_root_readme_and_architecture_docs_always_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/guide.md"), "# Guide\n\nbody\n").unwrap();
        std::fs::write(root.join("README.md"), "# Tokio\n\n## Overview\n\nbody\n").unwrap();
        std::fs::write(root.join("ARCHITECTURE.md"), "# Architecture\n\nbody\n").unwrap();
        // Root noise that must NOT be indexed implicitly.
        std::fs::write(root.join("notes.md"), "# Private notes\n").unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let opts = DocsIndexOptions {
            repo_root: root.to_path_buf(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: vec!["**/*.md".into()],
        };
        let result = index_docs(&mut store, &opts).unwrap();
        assert_eq!(
            result.files, 3,
            "docs/guide.md + README.md + ARCHITECTURE.md, not notes.md; got {result:?}"
        );
        let nodes = store.list_all_nodes().unwrap();
        assert!(
            nodes.iter().any(|n| n.id.to_string() == "file::README.md"),
            "root README must be indexed without configuration"
        );
        assert!(
            nodes
                .iter()
                .all(|n| n.id.to_string() != "file::notes.md"),
            "arbitrary root markdown must stay out"
        );
    }

    /// A README.rst root file (flask layout) gets the same treatment.
    #[test]
    fn repo_root_rst_readme_is_indexed_with_rst_headings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("README.rst"), "Flask\n=====\n\nbody\n").unwrap();

        let mut store = Store::open(root.join("graph.db")).unwrap();
        store.migrate().unwrap();
        let opts = DocsIndexOptions {
            repo_root: root.to_path_buf(),
            doc_roots: vec![PathBuf::from("docs")],
            include_globs: vec!["**/*.md".into(), "**/*.rst".into()],
        };
        let result = index_docs(&mut store, &opts).unwrap();
        assert_eq!(result.files, 1, "README.rst picked up from the repo root");
        let nodes = store.list_all_nodes().unwrap();
        assert!(
            nodes.iter().any(|n| n.name.as_deref() == Some("Flask")),
            "rst title of the root README must become a DocSection"
        );
    }
}
