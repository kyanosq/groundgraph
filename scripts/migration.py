#!/usr/bin/env python3
"""Local migration work packages and a fail-closed review gate (stdlib only).

prepare never changes an existing package. check --run-tests executes commands
from a developer-reviewed manifest, without a shell. Do not run untrusted manifests.
This verifies recorded scope/evidence, not arbitrary program equivalence.
"""
import argparse
import hashlib
import json
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path


def inside(root, relative):
    p = Path(relative)
    if p.is_absolute() or ".." in p.parts:
        raise ValueError(f"path must be relative: {relative}")
    resolved = (root / p).resolve()
    if not resolved.is_relative_to(root.resolve()):
        raise ValueError(f"path escapes root: {relative}")
    return resolved


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def pin(root, path):
    return {"path": path, "sha256": digest(inside(root, path))}


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def prepare(binary, source, paths, out):
    if out.exists():
        raise ValueError("output already exists; use a new snapshot, never overwrite reviewed mappings")
    paths = sorted(set(paths))
    if not paths:
        raise ValueError("select at least one source file")
    # Explicit allowlist only; no recursive scan of customer config or secrets.
    for path in paths:
        p = Path(path)
        if p.suffix not in {".java", ".xml", ".sql", ".ts", ".tsx", ".py", ".go", ".rs"}:
            raise ValueError(f"not a source file: {p}")
        if p.suffix == ".xml" and ET.parse(inside(source, str(p))).getroot().tag != "mapper":
            raise ValueError(f"only MyBatis mapper XML is allowed: {p}")
    pins = [pin(source, p) for p in paths]
    stage = out / "source"
    (stage / "src").mkdir(parents=True)
    for item in pins:
        destination = stage / "src" / item["path"]
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(inside(source, item["path"]).read_bytes())
        if digest(destination) != item["sha256"]:
            raise ValueError("source changed while preparing snapshot")
    # GroundGraph language adapters only see the selected snapshot.
    languages = {".java": "java", ".ts": "typescript", ".tsx": "typescript", ".py": "python", ".go": "go", ".rs": "rust"}
    ids = sorted({languages[Path(p["path"]).suffix] for p in pins if Path(p["path"]).suffix in languages})
    config = "repo:\n  root: .\n  default_branch: main\nstorage:\n  path: .groundgraph/graph.db\nlanguages:\n"
    config += "".join(f"  - id: {lang}\n    paths: [src]\n" for lang in ids)
    config += "enrichment:\n  scip: false\n  analyzer: false\n  lsp: false\n"
    # A selected slice has no verified build/module context. Retain inventory;
    # full-workspace javac analysis belongs to the explicitly configured source repo.
    config += "java_semantics:\n  enabled: false\n"
    (stage / ".groundgraph.yaml").write_text(config)
    binary = str(Path(binary).resolve())
    index = subprocess.run([binary, "--repo-root", str(stage), "index"], capture_output=True, text=True, timeout=300)
    (out / "index.log").write_text(index.stdout + index.stderr)
    if index.returncode:
        raise ValueError(f"index incomplete (exit {index.returncode}); see index.log")
    exported = subprocess.run([binary, "--repo-root", str(stage), "feature-pack", "--path", "src"], capture_output=True, text=True, timeout=300, check=True)
    pack = json.loads(exported.stdout)
    if pack.get("omitted_symbols"):
        raise ValueError("source symbols missing from export; inspect source snapshot")
    if not pack["symbols"]:
        raise ValueError("empty source inventory; no migration completion can be inferred")
    write_json(out / "feature-pack.json", pack)
    manifest = {
        "schema_version": 1,
        "feature_pack_sha256": digest(out / "feature-pack.json"),
        "source_files": pins,
        "review": {"status": "pending", "evidence": ""},
        "items": [{"id": f"item-{i + 1}", "sources": [s["id"]], "disposition": "unresolved", "reason": "", "targets": [], "checks": []} for i, s in enumerate(pack["symbols"])],
        "checks": [],
        "call_dispositions": [{"id": c["id"], "disposition": "unresolved", "reason": "", "targets": [], "checks": []}
                              for c in pack.get("java_analysis", {}).get("calls", []) if c.get("resolution") != "resolved"],
    }
    write_json(out / "migration.json", manifest)
    return {"source_files": len(pins), "source_symbols": len(pack["symbols"]), "manifest": str(out / "migration.json"), "status": "unresolved"}


def check(manifest, work, source, target, run_tests=False):
    blockers, runs = [], []
    source_pins, target_pins = [], []

    def verify(root, pins):
        for item in pins:
            try:
                if digest(inside(root, item["path"])) != item["sha256"]:
                    blockers.append(f"stale file: {item['path']}")
            except (OSError, ValueError, KeyError, TypeError) as e:
                blockers.append(f"invalid file evidence: {e}")

    try:
        if manifest["schema_version"] != 1:
            raise ValueError("unsupported manifest schema")
        pack_path = work / "feature-pack.json"
        if digest(pack_path) != manifest["feature_pack_sha256"]:
            raise ValueError("stale feature pack")
        pack = json.loads(pack_path.read_text())
        if pack.get("omitted_symbols"):
            raise ValueError("source symbols omitted from evidence pack")
        inventory = {s["id"] for s in pack["symbols"]}
        if not inventory or len(inventory) != len(pack["symbols"]):
            raise ValueError("empty or duplicate source inventory")
        source_pins = manifest["source_files"]
        if not source_pins:
            raise ValueError("missing source snapshot")
        if any(Path(p["path"]).suffix == ".java" for p in source_pins) and not isinstance(pack.get("java_analysis", {}).get("calls"), list):
            raise ValueError("missing Java call inventory; reindex the legacy evidence pack")
        verify(source, source_pins)
        # ponytail: local review/oracle attestations; require externally verified
        # approvals before using this workflow as an unattended publishing gate.
        if manifest.get("review", {}).get("status") != "reviewed" or not manifest["review"].get("evidence", "").strip():
            blockers.append("source behavior inventory and unresolved dependencies require review")
        checks = {c["id"]: c for c in manifest["checks"]}
        if len(checks) != len(manifest["checks"]):
            blockers.append("duplicate check id")
        covered, item_ids, needed = set(), set(), set()
        calls = pack.get("java_analysis", {}).get("calls", [])
        if len({c["id"] for c in calls}) != len(calls):
            raise ValueError("duplicate Java call site identity")
        unknown_calls = {c["id"] for c in calls if c.get("resolution") != "resolved"}
        dispositions = manifest.get("call_dispositions", [])
        disposition_ids = [d["id"] for d in dispositions]
        if len(set(disposition_ids)) != len(disposition_ids) or set(disposition_ids) != unknown_calls:
            blockers.append("unresolved call inventory must have exactly one disposition per call site")
        for item in dispositions:
            kind = item.get("disposition")
            if kind not in {"mapped", "platform", "excluded"} or not item.get("reason", "").strip():
                blockers.append(f"unresolved call disposition: {item['id']}")
            if kind in {"mapped", "platform"}:
                if not item.get("targets") or not item.get("checks"):
                    blockers.append(f"call disposition needs target artifacts and regression checks: {item['id']}")
                target_pins.extend(item.get("targets", []))
                needed.update(item.get("checks", []))
        counts = {"mapped": 0, "platform": 0, "excluded": 0, "unresolved": 0}
        for item in manifest["items"]:
            ident = item["id"]
            if ident in item_ids:
                blockers.append(f"duplicate mapping id: {ident}")
            item_ids.add(ident)
            ids = set(item["sources"])
            if not ids or not ids <= inventory:
                blockers.append(f"unknown or empty source mapping: {ident}")
            covered.update(ids)
            disposition = item["disposition"]
            if disposition not in counts:
                blockers.append(f"unknown disposition: {ident}")
                continue
            counts[disposition] += 1
            if disposition == "unresolved":
                blockers.append(f"unresolved mapping: {ident}")
            if not item.get("reason", "").strip():
                blockers.append(f"missing rationale: {ident}")
            if disposition in {"mapped", "platform"}:
                if not item.get("targets") or not item.get("checks"):
                    blockers.append(f"mapping needs target artifacts and regression checks: {ident}")
                target_pins.extend(item.get("targets", []))
                needed.update(item.get("checks", []))
        if inventory - covered:
            blockers.append(f"unmapped source symbols: {len(inventory - covered)}")
        for ident in sorted(needed):
            c = checks.get(ident)
            if not c:
                blockers.append(f"unknown regression check: {ident}")
                continue
            target_pins.extend(c.get("files", []))
            if not c.get("files"):
                blockers.append(f"test source must be pinned: {ident}")
            if c.get("oracle") not in {"legacy_execution", "business_approved"} or not c.get("oracle_evidence", "").strip():
                blockers.append(f"independent expected-result evidence missing: {ident}")
            argv = c.get("argv")
            if not isinstance(argv, list) or not argv or not all(isinstance(a, str) for a in argv) or not any("{junit}" in a for a in argv):
                blockers.append(f"check needs argv and fresh {{junit}} output: {ident}")
        verify(target, target_pins)
        if not run_tests and needed:
            blockers.append("regression checks not executed; use --run-tests after reviewing argv")
        if run_tests and not blockers:
            for ident in sorted(needed):
                c = checks[ident]
                runs_dir = work / "runs"
                runs_dir.mkdir(exist_ok=True)
                tmp = tempfile.mkdtemp(prefix="regression-", dir=runs_dir)
                receipt = Path(tmp) / "result.xml"
                argv = [a.replace("{junit}", str(receipt)) for a in c["argv"]]
                completed = subprocess.run(argv, cwd=target, capture_output=True, timeout=300)
                (Path(tmp) / "stdout.log").write_bytes(completed.stdout)
                (Path(tmp) / "stderr.log").write_bytes(completed.stderr)
                row = {"id": ident, "exit_code": completed.returncode, "passed": 0, "failed": 0, "skipped": 0, "artifacts": str(Path(tmp).relative_to(work))}
                if completed.returncode != 0:
                    blockers.append(f"regression command failed: {ident} (exit {completed.returncode})")
                if not receipt.is_file() or receipt.stat().st_size > 10_000_000:
                    blockers.append(f"fresh JUnit receipt missing or oversized: {ident}")
                else:
                    xml = ET.parse(receipt).getroot()
                    cases = list(xml.iter("testcase"))
                    row["failed"] = sum(case.find("failure") is not None or case.find("error") is not None for case in cases)
                    row["skipped"] = sum(case.find("skipped") is not None for case in cases)
                    row["passed"] = len(cases) - row["failed"] - row["skipped"]
                    row["receipt_sha256"] = digest(receipt)
                    # Some reporters record setup/collection failures only in suite counters.
                    suite_failed = any(int(suite.get(key, "0")) != 0 for suite in xml.iter() if suite.tag in {"testsuite", "testsuites"} for key in ("failures", "errors", "skipped", "disabled"))
                    if xml.tag not in {"testsuite", "testsuites"} or suite_failed or row["passed"] <= 0 or row["failed"] or row["skipped"] or list(xml.iter("error")) or list(xml.iter("failure")):
                        blockers.append(f"zero, failed or skipped regression tests: {ident}")
                runs.append(row)
            # A passing test run against a different source/target revision is stale.
            verify(source, source_pins)
            verify(target, target_pins)
        return {"ready_for_review": not blockers, "behavioral_equivalence": "not_proven", "scope_symbols": len(inventory), "accounted_symbols": len(inventory & covered), "dispositions": counts, "blockers": blockers, "test_runs": runs}
    except (OSError, ValueError, KeyError, TypeError, AttributeError, ET.ParseError, subprocess.SubprocessError) as e:
        blockers.append(f"invalid or incomplete evidence: {e}")
        return {"ready_for_review": False, "behavioral_equivalence": "not_proven", "blockers": blockers, "test_runs": runs}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("--groundgraph", required=True)
    p.add_argument("--source-root", type=Path, required=True)
    p.add_argument("--source-file", action="append", required=True)
    p.add_argument("--out", type=Path, required=True)
    c = sub.add_parser("check")
    c.add_argument("--manifest", type=Path, required=True)
    c.add_argument("--source-root", type=Path, required=True)
    c.add_argument("--target-root", type=Path, required=True)
    c.add_argument("--run-tests", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            result = prepare(args.groundgraph, args.source_root.resolve(), args.source_file, args.out.resolve())
            code = 0
        else:
            manifest_bytes = args.manifest.read_bytes()
            manifest_sha256 = hashlib.sha256(manifest_bytes).hexdigest()
            result = check(json.loads(manifest_bytes), args.manifest.resolve().parent, args.source_root.resolve(), args.target_root.resolve(), args.run_tests)
            result["manifest_sha256"] = manifest_sha256
            if digest(args.manifest) != manifest_sha256:
                result["ready_for_review"] = False
                result["blockers"].append("manifest changed during verification")
            code = 0 if result["ready_for_review"] else 2
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return code
    except (OSError, ValueError, subprocess.SubprocessError, ET.ParseError) as e:
        print(json.dumps({"error": str(e), "ready_for_review": False}, ensure_ascii=False))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
