//! Brand-level tests for the GroundGraph project name.

use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

#[test]
fn groundgraph_binary_is_the_primary_cli_name() {
    Command::cargo_bin("groundgraph")
        .expect("locate groundgraph binary")
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("groundgraph"))
        .stdout(contains("GroundGraph"))
        .stdout(contains("Primary binary: groundgraph."));
}

#[test]
fn cargo_manifest_does_not_ship_old_specslice_cli_alias() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))
        .expect("read groundgraph-cli manifest");

    assert!(
        !manifest.contains("name = \"specslice\""),
        "groundgraph-cli must not ship a specslice binary alias"
    );
    assert!(
        !manifest_dir.join("src/bin/specslice.rs").exists(),
        "old specslice binary entrypoint should be removed"
    );
}

#[test]
fn cargo_workspace_uses_groundgraph_package_names() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");

    let root_manifest = std::fs::read_to_string(workspace_root.join("Cargo.toml"))
        .expect("read workspace manifest");
    for member in [
        "groundgraph-core",
        "groundgraph-store",
        "groundgraph-lang-dart",
        "groundgraph-engine",
        "groundgraph-cli",
        "groundgraph-mcp",
    ] {
        assert!(
            root_manifest.contains(&format!("\"crates/{member}\"")),
            "workspace manifest should include crates/{member}"
        );
        let crate_manifest = std::fs::read_to_string(
            workspace_root
                .join("crates")
                .join(member)
                .join("Cargo.toml"),
        )
        .unwrap_or_else(|err| panic!("read crates/{member}/Cargo.toml: {err}"));
        assert!(
            crate_manifest.contains(&format!("name = \"{member}\"")),
            "crates/{member}/Cargo.toml should use package name {member}"
        );
    }

    assert!(
        !root_manifest.contains("crates/specslice-"),
        "workspace manifest should not keep specslice-* members"
    );
}
