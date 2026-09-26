import json
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace
from pathlib import Path

import migration


class MigrationGateTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        self.target = self.root / "target"
        self.work = self.root / "work"
        for p in (self.source, self.target, self.work):
            p.mkdir()
        (self.source / "Old.java").write_text("class Old {}")
        (self.target / "new.ts").write_text("export const price = 2;")
        (self.target / "test.py").write_text(
            "import sys\nfrom pathlib import Path\n"
            "Path(sys.argv[1]).write_text('<testsuite><testcase name=\"quote\"/></testsuite>')\n"
        )
        pack = {"symbols": [{"id": "old:quote"}, {"id": "old:check"}], "java_analysis": {"calls": []}}
        (self.work / "feature-pack.json").write_text(json.dumps(pack))
        self.manifest = {
            "schema_version": 1,
            "feature_pack_sha256": migration.digest(self.work / "feature-pack.json"),
            "source_files": [migration.pin(self.source, "Old.java")],
            "items": [{"id": "pricing", "sources": ["old:quote", "old:check"],
                       "disposition": "mapped", "reason": "Two source methods become one capability",
                       "targets": [migration.pin(self.target, "new.ts")], "checks": ["quote"]}],
            "checks": [{"id": "quote", "argv": [sys.executable, "test.py", "{junit}"],
                        "files": [migration.pin(self.target, "test.py")],
                        "oracle": "business_approved", "oracle_evidence": "approved fixture v1"}],
            "review": {"status": "reviewed", "evidence": "source slice and dependency boundary reviewed"},
        }

    def check(self, run=True):
        return migration.check(self.manifest, self.work, self.source, self.target, run)

    def test_many_to_many_mapping_requires_executed_tests(self):
        self.assertFalse(self.check(False)["ready_for_review"])
        result = self.check()
        self.assertTrue(result["ready_for_review"], result)
        self.assertEqual(result["test_runs"][0]["passed"], 1)
        self.assertEqual(result["behavioral_equivalence"], "not_proven")

    def test_missing_inventory_and_stale_source_block(self):
        self.manifest["items"][0]["sources"].pop()
        self.assertFalse(self.check()["ready_for_review"])
        (self.source / "Old.java").write_text("class Changed {}")
        self.assertTrue(any("stale" in e for e in self.check()["blockers"]))

    def test_exemptions_need_reasons_and_cannot_hide_unresolved(self):
        item = self.manifest["items"][0]
        item.update(disposition="excluded", reason="")
        self.assertFalse(self.check()["ready_for_review"])
        item.update(disposition="unresolved", reason="awaiting source")
        self.assertFalse(self.check()["ready_for_review"])

    def test_no_tests_failed_tests_and_code_derived_oracles_block(self):
        check = self.manifest["checks"][0]
        for xml in ['<testsuite/>', '<testsuite><testcase><failure/></testcase></testsuite>', '<testsuite><testcase><skipped/></testcase></testsuite>']:
            (self.target / "test.py").write_text("import sys\nfrom pathlib import Path\nPath(sys.argv[1]).write_text(" + repr(xml) + ")\n")
            check["files"] = [migration.pin(self.target, "test.py")]
            self.assertFalse(self.check()["ready_for_review"])
        check["oracle"] = "code_derived"
        self.assertFalse(self.check()["ready_for_review"])

    def test_suite_level_failure_counters_cannot_be_hidden(self):
        p = self.target / "test.py"
        p.write_text("import sys\nfrom pathlib import Path\nPath(sys.argv[1]).write_text('<testsuite failures=\"1\"><testcase/></testsuite>')\n")
        self.manifest["checks"][0]["files"] = [migration.pin(self.target, "test.py")]
        self.assertFalse(self.check()["ready_for_review"])

    def test_report_is_bound_to_exact_manifest_bytes(self):
        import subprocess
        manifest = self.work / "migration.json"
        migration.write_json(manifest, self.manifest)
        result = subprocess.run([sys.executable, migration.__file__, "check", "--manifest", str(manifest), "--source-root", str(self.source), "--target-root", str(self.target), "--run-tests"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout).get("manifest_sha256"), migration.digest(manifest))

    def test_stale_target_and_path_escape_block(self):
        (self.target / "new.ts").write_text("export const price = 200;")
        self.assertFalse(self.check()["ready_for_review"])
        with self.assertRaises(ValueError):
            migration.pin(self.source, "../target/new.ts")

    def test_duplicate_mapping_id_and_unreviewed_dependencies_block(self):
        self.manifest["items"].append(dict(self.manifest["items"][0]))
        self.assertFalse(self.check()["ready_for_review"])
        self.manifest["review"]["status"] = "pending"
        self.assertFalse(self.check()["ready_for_review"])

    def test_target_mutation_during_test_invalidates_result(self):
        p = self.target / "test.py"
        p.write_text(p.read_text() + "Path('new.ts').write_text('changed during test')\n")
        self.manifest["checks"][0]["files"] = [migration.pin(self.target, "test.py")]
        self.assertFalse(self.check()["ready_for_review"])

    def test_unresolved_calls_require_individual_disposition(self):
        path = self.work / "feature-pack.json"
        pack = json.loads(path.read_text())
        pack["java_analysis"] = {"calls": [{"id": "call:1", "resolution": "unresolved"}]}
        migration.write_json(path, pack)
        self.manifest["feature_pack_sha256"] = migration.digest(path)
        self.assertFalse(self.check()["ready_for_review"])
        self.manifest["call_dispositions"] = [{"id": "call:1", "disposition": "excluded", "reason": "Confirmed external telemetry, outside this migration"}]
        self.assertTrue(self.check()["ready_for_review"])

    def test_legacy_java_pack_without_call_inventory_blocks(self):
        path = self.work / "feature-pack.json"
        pack = json.loads(path.read_text())
        del pack["java_analysis"]
        migration.write_json(path, pack)
        self.manifest["feature_pack_sha256"] = migration.digest(path)
        self.assertFalse(self.check()["ready_for_review"])

    def test_partial_source_package_does_not_guess_compiler_visibility(self):
        out = self.root / "prepared"
        pack = json.loads((self.work / "feature-pack.json").read_text())
        with patch.object(migration.subprocess, "run", side_effect=[
            SimpleNamespace(returncode=0, stdout="", stderr=""),
            SimpleNamespace(stdout=json.dumps(pack)),
        ]):
            migration.prepare(self.root / "groundgraph", self.source, ["Old.java"], out)
        self.assertIn("java_semantics:\n  enabled: false", (out / "source/.groundgraph.yaml").read_text())


if __name__ == "__main__":
    unittest.main()
