//! Schema tests for `.specslice.yaml`.
//!
//! PRD §8 specifies a richer config (docs / code / links / slice / impact /
//! checks). MVP-0 shipped only `repo + storage`. These tests pin the
//! requirements:
//! 1. A minimal `repo + storage` config still parses.
//! 2. The PRD's full config parses without `deny_unknown_fields` errors.
//! 3. `index_repository` honours `docs.paths` and `code.paths` from config
//!    instead of hard-coding `docs/specs/adr` and `lib/test`.

use std::path::PathBuf;

use specslice_engine::config::{EngineConfig, DEFAULT_CONFIG_FILE_NAME};
use specslice_engine::impact::{run_impact, ImpactOptions};
use specslice_engine::index::{index_repository, IndexOptions};
use specslice_engine::init::{init_repository, InitOptions};
use specslice_engine::{run_checks, CheckOptions};
use specslice_store::Store;
use tempfile::TempDir;

fn write_config(repo_root: &std::path::Path, yaml: &str) {
    std::fs::write(repo_root.join(DEFAULT_CONFIG_FILE_NAME), yaml).unwrap();
}

#[test]
fn minimal_config_still_parses() {
    let yaml = "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\n";
    let cfg: EngineConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.repo.root, ".");
    assert_eq!(cfg.storage.path, ".specslice/graph.db");
    // Sections that were not provided fall back to defaults.
    assert!(!cfg.docs.paths.is_empty());
    assert!(!cfg.code.paths.is_empty());
    assert_eq!(cfg.dead_code.entrypoints, vec!["lib/main.dart"]);
    assert!(cfg.dead_code.ignore.contains(&"**/*.g.dart".to_string()));
    assert!(cfg
        .dead_code
        .ignore
        .contains(&"**/*.freezed.dart".to_string()));
    assert!(cfg
        .dead_code
        .ignore
        .contains(&"**/l10n/app_localizations*.dart".to_string()));
}

#[test]
fn full_prd_config_parses_without_deny_unknown_fields_error() {
    let yaml = r#"
repo:
  root: .
  default_branch: main

storage:
  path: .specslice/graph.db

docs:
  paths:
    - docs
    - specs
    - adr
  include:
    - "**/*.md"
    - "**/*.mdx"
  requirement_patterns:
    - "REQ-[A-Z]+-[0-9]+"
    - "AC-[A-Z]+-[0-9]+-[0-9]+"
  adr_patterns:
    - "ADR-[0-9]+"

code:
  language: dart
  paths:
    - lib
    - test
  adapter:
    backend: lightweight
  exclude:
    - .dart_tool
    - build
    - generated
    - "**/*.g.dart"
    - "**/*.freezed.dart"

links:
  path: .specslice/links.yaml

slice:
  max_depth: 3
  max_nodes: 120
  min_score: 0.35
  include_imports: false
  include_candidates: false

impact:
  auto_reindex_changed_files: true
  propagate_to_parent_symbol: true
  include_doc_changes: true
  stale_doc_level: info
  missing_test_change_level: warning

checks:
  broken_link_level: error
  missing_linked_test_level: warning
  orphan_requirement_level: warning
"#;
    let cfg: EngineConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.docs.paths.len(), 3);
    assert_eq!(cfg.code.paths.len(), 2);
    assert_eq!(cfg.code.exclude.len(), 5);
    assert_eq!(cfg.links.path, ".specslice/links.yaml");
    assert_eq!(cfg.checks.broken_link_level, "error");
}

#[test]
fn index_repository_honours_configured_docs_paths() {
    // Create a workspace where docs live under `requirements/`
    // rather than `docs/` — only succeeds if the engine reads `docs.paths`
    // from `.specslice.yaml` instead of hard-coding `docs/specs/adr`.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    // Overwrite the default config with one that points at `requirements/`.
    write_config(
        root,
        "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\ndocs:\n  paths:\n    - requirements\ncode:\n  paths: []\n",
    );
    std::fs::create_dir_all(root.join("requirements")).unwrap();
    std::fs::write(
        root.join("requirements/r1.md"),
        "---\nid: REQ-CFG-1\ntype: requirement\ntitle: T\n---\n\n# X\n",
    )
    .unwrap();
    // And legacy `docs/` content that must be ignored.
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("docs/legacy.md"),
        "---\nid: REQ-LEGACY-1\ntype: requirement\ntitle: L\n---\n# L\n",
    )
    .unwrap();

    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let docs = result.docs.expect("docs result present");
    // Only the requirements/ tree should have been scanned.
    assert_eq!(docs.files, 1);
    assert_eq!(docs.doc_sections, 1);
    assert_eq!(docs.requirements, 0);
    let store = Store::open(root.join(".specslice/graph.db")).unwrap();
    let sections = store
        .list_nodes_by_kind(specslice_core::NodeKind::DocSection)
        .unwrap();
    assert_eq!(sections.len(), 1);
    assert_eq!(
        sections[0].path.as_deref(),
        Some("requirements/r1.md"),
        "config-defined doc root must take precedence"
    );
}

#[test]
fn index_repository_honours_configured_code_paths_and_exclude() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    write_config(
        root,
        "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\ndocs:\n  paths: []\ncode:\n  paths:\n    - sources\n  exclude:\n    - \"**/*.g.dart\"\n",
    );
    std::fs::create_dir_all(root.join("sources/keep")).unwrap();
    std::fs::create_dir_all(root.join("sources/gen")).unwrap();
    std::fs::write(root.join("sources/keep/a.dart"), "class Keep {}\n").unwrap();
    std::fs::write(
        root.join("sources/gen/skip.g.dart"),
        "class GeneratedSkip {}\n",
    )
    .unwrap();
    // A file in the default `lib/` should be ignored because `code.paths`
    // does not include `lib`.
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("lib/ignored.dart"), "class Ignored {}\n").unwrap();

    index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let store = Store::open(root.join(".specslice/graph.db")).unwrap();
    let classes = store
        .list_nodes_by_kind(specslice_core::NodeKind::DartClass)
        .unwrap();
    let names: Vec<_> = classes.iter().map(|n| n.name.clone()).collect();
    assert!(names.iter().any(|n| n.as_deref() == Some("Keep")));
    assert!(!names.iter().any(|n| n.as_deref() == Some("GeneratedSkip")));
    assert!(!names.iter().any(|n| n.as_deref() == Some("Ignored")));
}

#[test]
fn index_repository_honours_configured_docs_include_globs() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    write_config(
        root,
        "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\ndocs:\n  paths:\n    - docs\n  include:\n    - \"**/*.spec.md\"\ncode:\n  paths: []\n",
    );
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("docs/keep.spec.md"),
        "---\nid: REQ-INCLUDE-1\ntype: requirement\ntitle: Keep\n---\n\n# Keep\n",
    )
    .unwrap();
    std::fs::write(
        root.join("docs/drop.md"),
        "---\nid: REQ-INCLUDE-2\ntype: requirement\ntitle: Drop\n---\n\n# Drop\n",
    )
    .unwrap();

    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let docs = result.docs.expect("docs result present");
    assert_eq!(docs.files, 1);
    assert_eq!(docs.doc_sections, 1);
    assert_eq!(docs.requirements, 0);
    let store = Store::open(root.join(".specslice/graph.db")).unwrap();
    let sections = store
        .list_nodes_by_kind(specslice_core::NodeKind::DocSection)
        .unwrap();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].path.as_deref(), Some("docs/keep.spec.md"));
}

#[test]
fn index_repository_honours_configured_links_manifest_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    write_config(
        root,
        "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\ndocs:\n  paths:\n    - docs\ncode:\n  paths:\n    - lib\nlinks:\n  path: links/custom.yaml\n",
    );
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("docs/r.md"),
        "---\nid: REQ-TAGS-1\ntype: requirement\ntitle: Tags\n---\n\n# Tags\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("lib/a.dart"), "class Tagged {}\n").unwrap();
    std::fs::create_dir_all(root.join("links")).unwrap();
    std::fs::write(
        root.join("links/custom.yaml"),
        "requirements:\n  REQ-TAGS-1:\n    implementations:\n      - lib/a.dart#Tagged\n",
    )
    .unwrap();

    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let code = result.code.expect("code result present");
    assert_eq!(code.declared_implementations, 0);
    let links = result.links.expect("links result present");
    assert_eq!(links.implementations, 1);
}

#[test]
fn run_checks_honours_configured_finding_levels() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    write_config(
        root,
        "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\ndocs:\n  paths: []\ncode:\n  paths: []\nchecks:\n  orphan_requirement_level: info\n  missing_linked_test_level: off\n",
    );
    let mut store = Store::open(root.join(".specslice/graph.db")).unwrap();
    let mut req = specslice_core::Node::new(
        specslice_core::artifact_id::requirement_id("REQ-CHECK-1"),
        specslice_core::NodeKind::Requirement,
    );
    req.path = Some("docs/check.md".into());
    store.upsert_node(&req).unwrap();
    drop(store);

    let report = run_checks(CheckOptions {
        repo_root: root.into(),
        impact: None,
    })
    .unwrap();
    let orphan = report
        .findings
        .iter()
        .find(|f| f.code == "orphan_requirement")
        .expect("orphan finding");
    assert_eq!(
        orphan.severity,
        specslice_engine::checks::CheckSeverity::Info
    );
}

#[test]
fn run_impact_honours_configured_doc_and_warning_levels() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    write_config(
        root,
        "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\ndocs:\n  paths: []\ncode:\n  paths: []\nimpact:\n  include_doc_changes: false\n  missing_test_change_level: off\n",
    );
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["init", "-q", "-b", "main"])
        .status()
        .unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "user.email", "t@t"])
        .status()
        .unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "user.name", "T"])
        .status()
        .unwrap();
    std::fs::write(root.join("note.md"), "one\n").unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["add", "."])
        .status()
        .unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "-q", "-m", "base"])
        .status()
        .unwrap();
    std::fs::write(root.join("note.md"), "two\n").unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["add", "."])
        .status()
        .unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "-q", "-m", "edit"])
        .status()
        .unwrap();

    let report = run_impact(ImpactOptions {
        repo_root: root.into(),
        base_ref: "HEAD~1".into(),
        head_ref: "HEAD".into(),
        reindex: true,
    })
    .unwrap();
    assert!(report.changed_doc_sections.is_empty());
    assert!(report.warnings.is_empty());
}

#[test]
fn default_config_serialises_with_new_sections_for_round_trip() {
    let cfg = EngineConfig::default();
    let yaml = serde_yaml::to_string(&cfg).unwrap();
    // Forward-compatibility: round-trip without losing the optional sections.
    let round_trip: EngineConfig = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(round_trip, cfg);
}

#[test]
fn p11_swift_and_go_sections_parse_with_paths_and_lsp_command() {
    let yaml = r#"
repo:
  root: .
  default_branch: main

storage:
  path: .specslice/graph.db

docs:
  paths: []
code:
  paths: []

swift:
  enabled: true
  paths:
    - Sources
    - Tests
  exclude:
    - "**/.build/**"
  lsp_command: /opt/swift/usr/bin/sourcekit-lsp

go:
  enabled: true
  paths:
    - .
    - cmd
  exclude:
    - "**/vendor/**"
"#;
    let cfg: EngineConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(cfg.swift.enabled, "swift.enabled must round-trip");
    assert_eq!(cfg.swift.paths_or(&["Sources"]), vec!["Sources", "Tests"]);
    assert_eq!(
        cfg.swift.lsp_command.as_deref(),
        Some("/opt/swift/usr/bin/sourcekit-lsp")
    );
    assert!(cfg.go.enabled);
    assert_eq!(cfg.go.paths_or(&["."]), vec![".", "cmd"]);
    assert_eq!(cfg.go.exclude, vec!["**/vendor/**".to_string()]);
    // Sections default to disabled when omitted.
    let minimal: EngineConfig = serde_yaml::from_str(
        "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\n",
    )
    .unwrap();
    assert!(!minimal.swift.enabled);
    assert!(!minimal.go.enabled);
}

#[test]
fn index_repository_skips_swift_adapter_when_disabled() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    // Default config does not enable swift / go.
    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    assert!(
        result.swift.is_none(),
        "swift result must be None when disabled"
    );
    assert!(result.go.is_none(), "go result must be None when disabled");
}

#[test]
fn index_repository_runs_swift_adapter_when_enabled_and_skips_when_lsp_missing() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    write_config(
        root,
        concat!(
            "repo:\n  root: .\n  default_branch: main\n",
            "storage:\n  path: .specslice/graph.db\n",
            "docs:\n  paths: []\n",
            "code:\n  paths: []\n",
            "swift:\n  enabled: true\n  paths: [Sources]\n  lsp_command: specslice_missing_sourcekit_lsp\n",
            "go:\n  enabled: true\n  paths: ['.']\n  lsp_command: specslice_missing_gopls\n",
        ),
    );
    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let swift = result.swift.expect("swift section present when enabled");
    assert_eq!(swift.files, 0);
    assert!(swift.sidecar_skip_reason.contains("PATH"));
    let go = result.go.expect("go section present when enabled");
    assert_eq!(go.files, 0);
    assert!(go.sidecar_skip_reason.contains("PATH"));
}

#[allow(dead_code)]
fn make_pathbuf(s: &str) -> PathBuf {
    PathBuf::from(s)
}
