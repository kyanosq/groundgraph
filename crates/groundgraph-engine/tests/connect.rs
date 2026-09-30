//! P1 integration tests: `groundgraph connect propose / apply` engine surface.
//!
//! The contract is file-driven so that AI generation stays external:
//! - `propose` reads the indexed graph and returns an `EvidencePack` (facts
//!   the AI should ground itself in). Markdown frontmatter is not semantic
//!   business logic; without confirmed links, requirements remain empty.
//! - `apply` reads a candidates YAML, validates each reference against the
//!   graph (existence / locatability) and merges accepted entries into
//!   `.groundgraph/links.yaml`. No rule matching may invent business links.

use std::path::Path;

use groundgraph_engine::connect::{apply_candidates, propose_evidence, ApplyOptions, ApplyOutcome};
use groundgraph_engine::index::{index_repository, IndexOptions};
use groundgraph_engine::init::{init_repository, InitOptions};
use tempfile::TempDir;

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn workspace_without_links() -> TempDir {
    let tmp = TempDir::new().unwrap();
    write(
        &tmp.path().join("docs/watermark.md"),
        "---\nid: REQ-WATERMARK-001\ntype: requirement\ntitle: Auto watermark placement\n---\n\n# Auto watermark placement\n\nThe watermark must avoid detected face regions.\n",
    );
    write(
        &tmp.path()
            .join("lib/domain/watermark/auto_placement_service.dart"),
        "class AutoPlacementService {\n  void placeWatermark() {}\n}\n",
    );
    write(
        &tmp.path()
            .join("test/watermark/auto_placement_service_test.dart"),
        "void main() {\n  test('places watermark outside face region', () {});\n}\n",
    );
    init_repository(InitOptions {
        repo_root: tmp.path().into(),
    })
    .unwrap();
    index_repository(IndexOptions::all(tmp.path())).unwrap();
    tmp
}

fn workspace_with_requirement() -> TempDir {
    let tmp = workspace_without_links();
    write(
        &tmp.path().join(".groundgraph/links.yaml"),
        "# Existing declaration, preserve these bytes on rejection.\nrequirements:\n  REQ-WATERMARK-001: {}\n",
    );
    index_repository(IndexOptions::all(tmp.path())).unwrap();
    tmp
}

fn candidate_yaml(version: &str, requirement: &str) -> String {
    format!(
        "{version}candidates:\n  - requirement: {requirement}\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n"
    )
}

fn assert_requirement_rejected(requirement: &str, reason: &str) {
    let tmp = workspace_with_requirement();
    let manifest = tmp.path().join(".groundgraph/links.yaml");
    let before = std::fs::read(&manifest).unwrap();
    let candidates = tmp.path().join("ai_candidates.yaml");
    write(&candidates, &candidate_yaml("", requirement));
    for dry_run in [false, true] {
        let outcome = apply_candidates(ApplyOptions {
            repo_root: tmp.path().into(),
            candidates_path: candidates.clone(),
            dry_run,
        })
        .unwrap();
        // Check preservation first: even a formatting-only rewrite is wrong.
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
        assert!(outcome.accepted.is_empty(), "{:?}", outcome.accepted);
        assert_eq!(outcome.rejected.len(), 1);
        assert!(outcome.rejected[0].reason.contains(reason), "{:?}", outcome);
        assert_eq!(outcome.dry_run, dry_run);
    }
}

#[test]
fn apply_rejects_unknown_requirement_without_rewriting_manifest() {
    assert_requirement_rejected("REQ-UNKNOWN", "cannot resolve requirement `REQ-UNKNOWN`");
}

#[test]
fn apply_rejects_empty_requirement_without_rewriting_manifest() {
    for requirement in ["\"\"", "\"  \\t  \""] {
        assert_requirement_rejected(requirement, "requirement must not be empty");
    }
}

#[test]
fn apply_refuses_future_candidate_schema_without_rewriting_manifest() {
    let tmp = workspace_with_requirement();
    let manifest = tmp.path().join(".groundgraph/links.yaml");
    let before = std::fs::read(&manifest).unwrap();
    let candidates = tmp.path().join("ai_candidates.yaml");
    for version in [2, u32::MAX] {
        for has_candidates in [false, true] {
            let yaml = if has_candidates {
                candidate_yaml(&format!("schema_version: {version}\n"), "REQ-WATERMARK-001")
            } else {
                format!("schema_version: {version}\ncandidates: []\n")
            };
            write(&candidates, &yaml);
            for dry_run in [false, true] {
                let result = apply_candidates(ApplyOptions {
                    repo_root: tmp.path().into(),
                    candidates_path: candidates.clone(),
                    dry_run,
                });
                assert_eq!(std::fs::read(&manifest).unwrap(), before);
                let error = result
                    .expect_err("future schema must be refused")
                    .to_string();
                assert!(
                    error.contains("unsupported candidates schema_version"),
                    "{error}"
                );
                assert!(error.contains(&version.to_string()), "{error}");
            }
        }
    }
}

#[test]
fn apply_accepts_legacy_and_current_candidate_schemas() {
    for version in ["", "schema_version: 0\n", "schema_version: 1\n"] {
        let tmp = workspace_with_requirement();
        let manifest = tmp.path().join(".groundgraph/links.yaml");
        let before = std::fs::read(&manifest).unwrap();
        let candidates = tmp.path().join("ai_candidates.yaml");
        write(&candidates, &candidate_yaml(version, "REQ-WATERMARK-001"));
        for dry_run in [true, false, false] {
            let outcome = apply_candidates(ApplyOptions {
                repo_root: tmp.path().into(),
                candidates_path: candidates.clone(),
                dry_run,
            })
            .unwrap();
            assert_eq!(outcome.accepted.len(), 1, "{version}: {outcome:?}");
            assert!(outcome.rejected.is_empty());
            assert_eq!(outcome.dry_run, dry_run);
            if dry_run {
                assert_eq!(std::fs::read(&manifest).unwrap(), before);
            } else {
                let yaml: serde_norway::Value =
                    serde_norway::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
                let refs = yaml["requirements"]["REQ-WATERMARK-001"]["implementations"]
                    .as_sequence()
                    .unwrap();
                assert_eq!(refs.len(), 1, "repeated apply must deduplicate");
            }
        }
    }
}

#[test]
fn apply_mixed_requirements_only_merges_known_requirement() {
    let tmp = workspace_with_requirement();
    let manifest = tmp.path().join(".groundgraph/links.yaml");
    let before = std::fs::read(&manifest).unwrap();
    let candidates = tmp.path().join("ai_candidates.yaml");
    write(
        &candidates,
        &(candidate_yaml("schema_version: 1\n", "REQ-WATERMARK-001")
            + "  - requirement: REQ-UNKNOWN\n    docs:\n      - docs/watermark.md#auto-watermark-placement\n"),
    );
    for dry_run in [true, false] {
        let outcome = apply_candidates(ApplyOptions {
            repo_root: tmp.path().into(),
            candidates_path: candidates.clone(),
            dry_run,
        })
        .unwrap();
        assert_eq!(outcome.accepted.len(), 1);
        assert_eq!(outcome.rejected.len(), 1);
        assert_eq!(outcome.rejected[0].requirement, "REQ-UNKNOWN");
        if dry_run {
            assert_eq!(std::fs::read(&manifest).unwrap(), before);
        } else {
            let raw = std::fs::read_to_string(&manifest).unwrap();
            assert!(raw.contains("AutoPlacementService"));
            assert!(!raw.contains("REQ-UNKNOWN"));
        }
    }
}

#[test]
fn propose_does_not_infer_requirements_from_markdown_frontmatter() {
    let tmp = workspace_without_links();

    let pack = propose_evidence(tmp.path()).unwrap();

    assert_eq!(pack.schema_version, 1);
    assert!(
        pack.requirements.is_empty(),
        "AI, not markdown rules, creates business logic candidates"
    );
    assert!(!pack.orphan_doc_sections.is_empty());
}

#[test]
fn propose_surfaces_orphan_symbols_and_tests_as_candidates_for_ai() {
    let tmp = workspace_without_links();

    let pack = propose_evidence(tmp.path()).unwrap();

    let class = pack
        .orphan_symbols
        .iter()
        .find(|s| s.name == "AutoPlacementService")
        .expect("class is orphan candidate");
    assert_eq!(
        class.path,
        "lib/domain/watermark/auto_placement_service.dart"
    );
    assert!(class.line_range.is_some(), "{:?}", class);

    let test = pack
        .orphan_tests
        .iter()
        .find(|t| t.name.contains("places watermark"))
        .expect("test case is orphan candidate");
    assert_eq!(test.path, "test/watermark/auto_placement_service_test.dart");
}

#[test]
fn apply_writes_validated_candidates_into_links_manifest() {
    let tmp = workspace_with_requirement();
    let candidates = tmp.path().join("ai_candidates.yaml");
    std::fs::write(
        &candidates,
        "candidates:\n  - requirement: REQ-WATERMARK-001\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n    tests:\n      - test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region\n    confidence: 0.9\n    rationale: ai-generated\n",
    )
    .unwrap();

    let outcome: ApplyOutcome = apply_candidates(ApplyOptions {
        repo_root: tmp.path().into(),
        candidates_path: candidates,
        dry_run: false,
    })
    .unwrap();

    assert_eq!(outcome.accepted.len(), 1);
    assert!(outcome.rejected.is_empty(), "{:?}", outcome.rejected);
    assert!(!outcome.dry_run);

    let manifest = std::fs::read_to_string(tmp.path().join(".groundgraph/links.yaml")).unwrap();
    assert!(manifest.contains("REQ-WATERMARK-001"));
    assert!(manifest.contains("auto_placement_service.dart#AutoPlacementService"));
    assert!(
        manifest.contains("auto_placement_service_test.dart#places-watermark-outside-face-region")
    );

    // Re-indexing the workspace must now produce the declared edges.
    let reindex = index_repository(IndexOptions::all(tmp.path())).unwrap();
    let links = reindex.links.unwrap();
    assert_eq!(links.implementations, 1);
    assert_eq!(links.tests, 1);
}

#[test]
fn apply_rejects_candidates_whose_targets_are_not_locatable() {
    let tmp = workspace_without_links();
    let candidates = tmp.path().join("ai_candidates.yaml");
    std::fs::write(
        &candidates,
        "candidates:\n  - requirement: REQ-WATERMARK-001\n    implementations:\n      - lib/domain/watermark/ghost.dart#GhostService\n",
    )
    .unwrap();

    let outcome = apply_candidates(ApplyOptions {
        repo_root: tmp.path().into(),
        candidates_path: candidates,
        dry_run: false,
    })
    .unwrap();

    assert!(outcome.accepted.is_empty(), "{:?}", outcome.accepted);
    assert_eq!(outcome.rejected.len(), 1);
    let rejected = &outcome.rejected[0];
    assert_eq!(rejected.requirement, "REQ-WATERMARK-001");
    assert!(
        rejected.reason.contains("ghost.dart"),
        "{}",
        rejected.reason
    );

    // The manifest is left at its init-time state (no candidates accepted means
    // no write).
    let manifest = std::fs::read_to_string(tmp.path().join(".groundgraph/links.yaml")).unwrap();
    assert!(
        !manifest.contains("REQ-WATERMARK-001"),
        "manifest unexpectedly mutated: {manifest}"
    );
}

#[test]
fn apply_dry_run_validates_without_writing_manifest() {
    let tmp = workspace_with_requirement();
    let candidates = tmp.path().join("ai_candidates.yaml");
    std::fs::write(
        &candidates,
        "candidates:\n  - requirement: REQ-WATERMARK-001\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n",
    )
    .unwrap();

    let outcome = apply_candidates(ApplyOptions {
        repo_root: tmp.path().into(),
        candidates_path: candidates,
        dry_run: true,
    })
    .unwrap();

    assert!(outcome.dry_run);
    assert_eq!(outcome.accepted.len(), 1);
    let manifest = std::fs::read_to_string(tmp.path().join(".groundgraph/links.yaml")).unwrap();
    assert!(
        !manifest.contains("AutoPlacementService"),
        "dry-run mutated manifest: {manifest}"
    );
}

#[test]
fn apply_merges_into_existing_manifest_without_clobbering_other_requirements() {
    let tmp = workspace_with_requirement();
    write(
        &tmp.path().join(".groundgraph/links.yaml"),
        "requirements:\n  REQ-OTHER-001:\n    docs:\n      - docs/watermark.md#auto-watermark-placement\n",
    );
    let candidates = tmp.path().join("ai_candidates.yaml");
    std::fs::write(
        &candidates,
        "candidates:\n  - requirement: REQ-WATERMARK-001\n    implementations:\n      - lib/domain/watermark/auto_placement_service.dart#AutoPlacementService\n",
    )
    .unwrap();

    let outcome = apply_candidates(ApplyOptions {
        repo_root: tmp.path().into(),
        candidates_path: candidates,
        dry_run: false,
    })
    .unwrap();
    assert_eq!(outcome.accepted.len(), 1);

    let manifest = std::fs::read_to_string(tmp.path().join(".groundgraph/links.yaml")).unwrap();
    assert!(manifest.contains("REQ-OTHER-001"));
    assert!(manifest.contains("REQ-WATERMARK-001"));
}

#[test]
fn apply_rejects_candidates_that_reference_other_requirement_via_unknown_doc() {
    let tmp = workspace_without_links();
    let candidates = tmp.path().join("ai_candidates.yaml");
    std::fs::write(
        &candidates,
        "candidates:\n  - requirement: REQ-WATERMARK-001\n    docs:\n      - docs/missing.md#section\n",
    )
    .unwrap();

    let outcome = apply_candidates(ApplyOptions {
        repo_root: tmp.path().into(),
        candidates_path: candidates,
        dry_run: false,
    })
    .unwrap();

    assert!(outcome.accepted.is_empty());
    assert_eq!(outcome.rejected.len(), 1);
    assert!(outcome.rejected[0].reason.contains("missing.md"));
}
