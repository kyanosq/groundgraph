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

use specslice_engine::config::{
    config_schema_notice, EngineConfig, CONFIG_SCHEMA_VERSION, DEFAULT_CONFIG_FILE_NAME,
};
use specslice_engine::impact::{run_impact, ImpactOptions};
use specslice_engine::index::{index_repository, IndexOptions};
use specslice_engine::init::{init_repository, InitOptions};
use specslice_engine::{run_checks, CheckOptions};
use specslice_store::Store;
use tempfile::TempDir;

fn write_config(repo_root: &std::path::Path, yaml: &str) {
    std::fs::write(repo_root.join(DEFAULT_CONFIG_FILE_NAME), yaml).unwrap();
}

/// Run a git command and assert it succeeded (#78). Without the success
/// assertion a non-zero git exit (lock file, hook, detached HEAD) would let the
/// test proceed on a broken repo and fail elsewhere with a misleading message.
fn run_git(repo: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed with {status}");
}

#[test]
fn minimal_config_still_parses() {
    let yaml = "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\n";
    let cfg: EngineConfig = serde_yml::from_str(yaml).unwrap();
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
    let cfg: EngineConfig = serde_yml::from_str(yaml).unwrap();
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
    run_git(root, &["init", "-q", "-b", "main"]);
    run_git(root, &["config", "user.email", "t@t"]);
    run_git(root, &["config", "user.name", "T"]);
    std::fs::write(root.join("note.md"), "one\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "base"]);
    std::fs::write(root.join("note.md"), "two\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "edit"]);

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
    let yaml = serde_yml::to_string(&cfg).unwrap();
    // Forward-compatibility: round-trip without losing the optional sections.
    let round_trip: EngineConfig = serde_yml::from_str(&yaml).unwrap();
    assert_eq!(round_trip, cfg);
}

/// #72: the forward-compat guard warns *only* when the declared schema version
/// is strictly newer than this build supports. Legacy (unversioned) files and
/// any version at-or-below the supported one pass silently.
#[test]
fn config_schema_notice_warns_only_on_newer_than_supported() {
    assert!(
        config_schema_notice(None, 1).is_none(),
        "a legacy file with no schema_version must not warn"
    );
    assert!(
        config_schema_notice(Some(1), 1).is_none(),
        "the exact supported version must not warn"
    );
    assert!(
        config_schema_notice(Some(0), 1).is_none(),
        "an older version must not warn (this build understands it)"
    );
    let notice = config_schema_notice(Some(2), 1).expect("a newer version must warn");
    assert!(
        notice.contains('2') && notice.contains('1'),
        "names both versions: {notice}"
    );
    assert!(
        notice.contains("schema_version"),
        "names the field so the operator can find it: {notice}"
    );
}

/// #72: a config without the field parses (legacy) and emits no notice.
#[test]
fn legacy_config_without_schema_version_loads_and_is_silent() {
    let yaml = "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\n";
    let cfg: EngineConfig = serde_yml::from_str(yaml).unwrap();
    assert_eq!(cfg.schema_version, None);
    assert!(cfg.schema_version_notice().is_none());
}

/// #72: `init` stamps every freshly-written `.specslice.yaml` with the current
/// schema version (Dart and polyglot branches both), so future builds can
/// detect version skew. Mirrors how the DB and every other contract is versioned.
#[test]
fn init_stamps_config_schema_version() {
    // Dart branch (legacy `code` default).
    let dart = TempDir::new().unwrap();
    std::fs::write(dart.path().join("pubspec.yaml"), "name: x\n").unwrap();
    std::fs::create_dir_all(dart.path().join("lib")).unwrap();
    std::fs::write(dart.path().join("lib/main.dart"), "void main() {}\n").unwrap();
    init_repository(InitOptions {
        repo_root: dart.path().into(),
    })
    .unwrap();
    let cfg: EngineConfig = serde_yml::from_str(
        &std::fs::read_to_string(dart.path().join(DEFAULT_CONFIG_FILE_NAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(cfg.schema_version, Some(CONFIG_SCHEMA_VERSION));
    assert!(
        cfg.languages.is_empty(),
        "Dart still uses the legacy default"
    );

    // Polyglot branch (unified `languages` list).
    let poly = TempDir::new().unwrap();
    std::fs::create_dir_all(poly.path().join("Sources")).unwrap();
    std::fs::write(
        poly.path().join("Package.swift"),
        "// swift-tools-version:5.9\n",
    )
    .unwrap();
    std::fs::write(
        poly.path().join("Sources/Engine.swift"),
        "struct Engine {}\n",
    )
    .unwrap();
    init_repository(InitOptions {
        repo_root: poly.path().into(),
    })
    .unwrap();
    let cfg: EngineConfig = serde_yml::from_str(
        &std::fs::read_to_string(poly.path().join(DEFAULT_CONFIG_FILE_NAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(cfg.schema_version, Some(CONFIG_SCHEMA_VERSION));
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
    let cfg: EngineConfig = serde_yml::from_str(yaml).unwrap();
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
    let minimal: EngineConfig = serde_yml::from_str(
        "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .specslice/graph.db\n",
    )
    .unwrap();
    assert!(!minimal.swift.enabled);
    assert!(!minimal.go.enabled);
}

#[test]
fn init_autodetects_swift_repo_and_writes_treesitter_config() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("App/Views")).unwrap();
    std::fs::create_dir_all(root.join("Sources/Core")).unwrap();
    std::fs::write(root.join("Package.swift"), "// swift-tools-version:5.9\n").unwrap();
    std::fs::write(
        root.join("App/Views/HomeView.swift"),
        "struct HomeView {}\n",
    )
    .unwrap();
    std::fs::write(root.join("Sources/Core/Engine.swift"), "struct Engine {}\n").unwrap();

    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();

    let cfg: EngineConfig =
        serde_yml::from_str(&std::fs::read_to_string(root.join(DEFAULT_CONFIG_FILE_NAME)).unwrap())
            .unwrap();
    assert_eq!(cfg.languages.len(), 1, "exactly one detected language");
    assert_eq!(cfg.languages[0].id, "swift");
    assert!(
        cfg.languages[0].paths.contains(&"App".to_string()),
        "detected source dir App: {:?}",
        cfg.languages[0].paths
    );
    assert!(
        cfg.languages[0].paths.contains(&"Sources".to_string()),
        "detected source dir Sources: {:?}",
        cfg.languages[0].paths
    );
    assert!(
        !cfg.enrichment.lsp,
        "zero-config init uses the always-available tree-sitter backend"
    );
    // The unified `languages` list must route Swift through the generic
    // tree-sitter driver (no external sourcekit-lsp needed to get a graph).
    let norm = cfg.normalized();
    assert!(norm.treesitter.enabled, "tree-sitter backend enabled");
    assert!(norm.treesitter.languages.contains(&"swift".to_string()));
}

#[test]
fn init_on_dart_repo_keeps_legacy_default() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("pubspec.yaml"), "name: x\n").unwrap();
    std::fs::write(root.join("lib/main.dart"), "void main() {}\n").unwrap();

    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();

    let cfg: EngineConfig =
        serde_yml::from_str(&std::fs::read_to_string(root.join(DEFAULT_CONFIG_FILE_NAME)).unwrap())
            .unwrap();
    assert!(
        cfg.languages.is_empty(),
        "Dart keeps the legacy code-section default, not a languages list"
    );
    assert_eq!(cfg.code.language, "dart");
    assert_eq!(cfg.code.paths, vec!["lib".to_string(), "test".to_string()]);
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
    // #187 gates a repo-provided `swift.lsp_command` behind a trust env, so a
    // bogus command in `.specslice.yaml` is *ignored* and the adapter falls
    // back to the default `sourcekit-lsp` — which exists on a macOS dev box,
    // making this test's "lsp missing" premise machine-dependent. The operator
    // override `SPECSLICE_SWIFT_LSP_BIN` is always honoured (it bypasses the
    // gate), so inject a guaranteed-missing binary through it to exercise the
    // "missing → skip with a PATH reason" path deterministically on any host.
    // Set once per process and never restored: env is global and `cargo test`
    // is multi-threaded; only this (swift-enabled) test reads the variable, so
    // a single write under `Once` cannot race a removal (#65).
    static SET_MISSING_SWIFT_LSP: std::sync::Once = std::sync::Once::new();
    SET_MISSING_SWIFT_LSP.call_once(|| {
        std::env::set_var(
            specslice_engine::SWIFT_LSP_COMMAND_ENV,
            "specslice_missing_sourcekit_lsp",
        );
    });

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
            "swift:\n  enabled: true\n  paths: [Sources]\n",
            "go:\n  enabled: true\n  paths: ['.']\n",
        ),
    );
    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    // Swift keeps the LSP sidecar: a missing binary surfaces a skip reason.
    let swift = result.swift.expect("swift section present when enabled");
    assert_eq!(swift.files, 0);
    assert!(swift.sidecar_skip_reason.contains("PATH"));
    // Go retired its LSP sidecar: the structure+heuristic adapter just runs and
    // finds zero Go files in the empty temp repo (no skip-reason concept).
    let go = result.go.expect("go section present when enabled");
    assert_eq!(go.files, 0);
}

#[test]
fn p23_unified_languages_normalize_onto_legacy_switches() {
    use specslice_engine::config::LanguageSelection;
    let cfg = EngineConfig {
        languages: vec![
            LanguageSelection {
                id: "dart".into(),
                paths: vec!["sources".into()],
                exclude: vec!["**/*.g.dart".into()],
                lsp_command: None,
            },
            LanguageSelection {
                id: "rust".into(),
                paths: vec!["crates".into()],
                ..Default::default()
            },
            LanguageSelection {
                id: "swift".into(),
                paths: vec!["Sources".into()],
                lsp_command: Some("sklsp".into()),
                ..Default::default()
            },
            LanguageSelection {
                id: "ts".into(),
                ..Default::default()
            },
            LanguageSelection {
                id: "c++".into(),
                paths: vec!["native".into()],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let n = cfg.normalized();
    // dart → code (canonical Dart structural/analyzer path).
    assert_eq!(n.code.paths, vec!["sources".to_string()]);
    assert_eq!(n.code.exclude, vec!["**/*.g.dart".to_string()]);
    // rust → rust adapter (no LSP tier; always tree-sitter).
    assert!(n.rust.enabled);
    assert_eq!(n.rust.paths, vec!["crates".to_string()]);
    // swift → Tier-3 adapter (enrichment.lsp defaults true).
    assert!(n.swift.enabled);
    assert_eq!(n.swift.lsp_command.as_deref(), Some("sklsp"));
    // ts alias → typescript adapter.
    assert!(n.typescript.enabled);
    // c++ alias → generic tree-sitter structural driver.
    assert!(n.treesitter.enabled);
    assert!(n.treesitter.languages.contains(&"cpp".to_string()));
    assert!(n.treesitter.paths.contains(&"native".to_string()));
    // The canonical list is consumed; re-normalising is a no-op (idempotent).
    assert!(n.languages.is_empty());
    assert_eq!(n.clone().normalized(), n);
}

#[test]
fn javascript_alias_routes_through_typescript_adapter() {
    use specslice_engine::config::{canonical_language_id, EngineConfig, LanguageSelection};
    // `javascript` / `js` are parsed by the JSX-aware TypeScript grammar, so
    // they canonicalise to `typescript` and run through its adapter (which
    // already indexes `.js` / `.jsx` / `.mjs` / `.cjs`).
    assert_eq!(canonical_language_id("javascript"), Some("typescript"));
    assert_eq!(canonical_language_id("js"), Some("typescript"));

    let cfg = EngineConfig {
        languages: vec![LanguageSelection {
            id: "javascript".into(),
            paths: vec!["web".into()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let n = cfg.normalized();
    assert!(
        n.typescript.enabled,
        "javascript selection enables the typescript adapter"
    );
    assert_eq!(n.typescript.paths, vec!["web".to_string()]);
}

#[test]
fn p23_enrichment_lsp_false_routes_lsp_languages_to_structure_only() {
    use specslice_engine::config::{EnrichmentConfig, LanguageSelection};
    let cfg = EngineConfig {
        enrichment: EnrichmentConfig {
            lsp: false,
            analyzer: true,
            scip: true,
        },
        languages: vec![
            LanguageSelection {
                id: "swift".into(),
                paths: vec!["Sources".into()],
                ..Default::default()
            },
            LanguageSelection {
                id: "go".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let n = cfg.normalized();
    // With LSP enrichment off, the Tier-3 adapters stay disabled and the
    // languages are indexed structurally by the generic tree-sitter driver.
    assert!(
        !n.swift.enabled,
        "swift LSP adapter off when enrichment.lsp=false"
    );
    assert!(
        !n.go.enabled,
        "go LSP adapter off when enrichment.lsp=false"
    );
    assert!(n.treesitter.enabled);
    assert!(n.treesitter.languages.contains(&"swift".to_string()));
    assert!(n.treesitter.languages.contains(&"go".to_string()));
    // `languages` is authoritative: Dart was not listed, so it is excluded
    // (empty code root scans nothing) even though it has no `enabled` flag.
    assert!(
        n.code.paths.is_empty(),
        "dart must be excluded when not listed in `languages`"
    );
}

#[test]
fn p23_unified_languages_yaml_parses_and_indexes_dart() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init_repository(InitOptions {
        repo_root: root.into(),
    })
    .unwrap();
    // Canonical unified selector: index Dart from `sources/` only.
    write_config(
        root,
        concat!(
            "repo:\n  root: .\n  default_branch: main\n",
            "storage:\n  path: .specslice/graph.db\n",
            "docs:\n  paths: []\n",
            "languages:\n",
            "  - id: dart\n",
            "    paths: [sources]\n",
            "    exclude: [\"**/*.g.dart\"]\n",
        ),
    );
    std::fs::create_dir_all(root.join("sources")).unwrap();
    std::fs::write(root.join("sources/a.dart"), "class Keep {}\n").unwrap();
    std::fs::write(root.join("sources/skip.g.dart"), "class GeneratedSkip {}\n").unwrap();
    // A file under the default `lib/` must be ignored — `languages` is
    // authoritative and points only at `sources/`.
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("lib/ignored.dart"), "class Ignored {}\n").unwrap();

    index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    let store = Store::open(root.join(".specslice/graph.db")).unwrap();
    let classes = store
        .list_nodes_by_kind(specslice_core::NodeKind::DartClass)
        .unwrap();
    let names: Vec<_> = classes.iter().filter_map(|n| n.name.clone()).collect();
    assert!(names.iter().any(|n| n == "Keep"), "got {names:?}");
    assert!(!names.iter().any(|n| n == "GeneratedSkip"));
    assert!(!names.iter().any(|n| n == "Ignored"));
}

#[test]
fn p23_unified_languages_excludes_unlisted_dart() {
    // P23.10 dogfood regression: a unified config that lists only `rust` must
    // NOT scan Dart at all, even though `.dart` files exist in the repo. The
    // previous behaviour fell back to scanning `.` (the whole repo) when the
    // Dart code root was emptied, indexing hundreds of fixture Dart files.
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
            "languages:\n",
            "  - id: rust\n",
            "    paths: [crates]\n",
            "enrichment:\n  lsp: false\n  analyzer: false\n",
        ),
    );
    // A Dart file that the default `lib/` root would have picked up.
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("lib/widget.dart"), "class Widget {}\n").unwrap();
    // A real Rust symbol to prove the listed language still indexes.
    std::fs::create_dir_all(root.join("crates/demo/src")).unwrap();
    std::fs::write(
        root.join("crates/demo/src/lib.rs"),
        "pub fn demo_entry() {}\n",
    )
    .unwrap();

    let result = index_repository(IndexOptions::all(root.to_path_buf())).unwrap();
    // Dart is unlisted → its pass is skipped entirely (no `code` result, and
    // certainly zero scanned files).
    assert_eq!(
        result.code.map_or(0, |c| c.files),
        0,
        "no Dart files scanned"
    );

    let store = Store::open(root.join(".specslice/graph.db")).unwrap();
    assert!(
        store
            .list_nodes_by_kind(specslice_core::NodeKind::DartClass)
            .unwrap()
            .is_empty(),
        "Dart must be fully excluded when unlisted"
    );
    let rust_fns = store
        .list_nodes_by_kind(specslice_core::NodeKind::RustFunction)
        .unwrap();
    assert!(
        rust_fns
            .iter()
            .any(|n| n.name.as_deref() == Some("demo_entry")),
        "listed Rust language still indexed"
    );
}

#[allow(dead_code)]
fn make_pathbuf(s: &str) -> PathBuf {
    PathBuf::from(s)
}
