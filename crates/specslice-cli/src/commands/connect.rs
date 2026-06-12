use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use specslice_engine::{
    apply_candidates, propose_evidence, ApplyOptions, ApplyOutcome, EvidencePack,
};

pub fn run_propose(repo_root: &Path, out: Option<&Path>, pretty: bool) -> Result<()> {
    let pack = propose_evidence(repo_root)?;
    let serialised = serialise_evidence(&pack, pretty)?;
    match out {
        Some(path) => super::output::write_atomic(path, &serialised)?,
        None => println!("{}", serialised),
    }
    Ok(())
}

pub fn run_apply(repo_root: &Path, candidates: PathBuf, dry_run: bool, json: bool) -> Result<i32> {
    let outcome = apply_candidates(ApplyOptions {
        repo_root: repo_root.to_path_buf(),
        candidates_path: candidates,
        dry_run,
    })?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome).context("serialising apply outcome to JSON")?
        );
    } else {
        print_human(&outcome);
    }
    Ok(
        if outcome.rejected.is_empty() && (!outcome.accepted.is_empty() || dry_run) {
            0
        } else if outcome.accepted.is_empty() && !outcome.rejected.is_empty() {
            // Nothing landed because every candidate was rejected.
            2
        } else {
            // Mixed: some accepted, some rejected. Non-zero so operators notice.
            1
        },
    )
}

fn serialise_evidence(pack: &EvidencePack, pretty: bool) -> Result<String> {
    let json = if pretty {
        serde_json::to_string_pretty(pack)
    } else {
        serde_json::to_string(pack)
    };
    json.context("serialising evidence pack to JSON")
}

fn print_human(outcome: &ApplyOutcome) {
    let suffix = if outcome.dry_run { " (dry run)" } else { "" };
    println!(
        "SpecSlice Connect{suffix}: {} accepted, {} rejected (manifest: {}).",
        outcome.accepted.len(),
        outcome.rejected.len(),
        outcome.manifest_path,
    );
    if !outcome.accepted.is_empty() {
        println!("\nAccepted:");
        for c in &outcome.accepted {
            println!("- {}", c.requirement);
            for d in &c.docs {
                println!("    doc:  {d}");
            }
            for i in &c.implementations {
                println!("    impl: {i}");
            }
            for t in &c.tests {
                println!("    test: {t}");
            }
        }
    }
    if !outcome.rejected.is_empty() {
        println!("\nRejected:");
        for r in &outcome.rejected {
            println!("- {} — {}", r.requirement, r.reason);
        }
    }
}
