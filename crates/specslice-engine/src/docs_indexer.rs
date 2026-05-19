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
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let is_md = matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("md") | Some("mdx")
            );
            if !is_md {
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
    Ok(result)
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
    let parsed = parse_markdown(&raw);

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
}
