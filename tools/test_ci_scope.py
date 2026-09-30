"""保证范围优化不会遗漏跨组件改动和无法比较的提交。"""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("ci_scope", Path(__file__).with_name("ci-scope.py"))
scope = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scope)
ALL = {"backend", "frontend", "worker"}


class CiScopeTests(unittest.TestCase):
    def test_component_changes_and_mixed_commit(self):
        for path, expected in (("backend/Cargo.lock", "backend"), ("frontend/src/main.ts", "frontend"), ("services/egress/bridge.py", "worker"), ("plugins/excel-bridge/requirements.lock", "worker")):
            self.assertEqual(scope.classify([path]), {expected})
        self.assertEqual(scope.classify(["backend/a.rs", "frontend/a.vue"]), {"backend", "frontend"})

    def test_docs_only_and_deleted_or_renamed_source(self):
        self.assertEqual(scope.classify(["docs/api.md", "README.md"]), set())
        self.assertEqual(scope.classify(["backend/deleted.rs", "docs/moved.rs"]), {"backend"})

    def test_workflow_release_and_unknown_changes_run_all(self):
        for path in (".github/workflows/ci.yml", "release/version.yaml", "deploy/Dockerfile", "new-config"):
            self.assertEqual(scope.classify([path]), ALL)

    def run_event(self, name, event, diff=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "event.json").write_text(json.dumps(event))
            with patch.dict(os.environ, {"GITHUB_EVENT_NAME": name, "GITHUB_EVENT_PATH": str(root / "event.json"), "GITHUB_OUTPUT": str(root / "output"), "GITHUB_SHA": "head"}), patch.object(subprocess, "check_output", side_effect=diff) as command:
                scope.main()
                return (root / "output").read_text(), command.call_args

    def test_manual_and_missing_base_run_all(self):
        for name, event, result in (("workflow_dispatch", {}, None), ("push", {"before": "0" * 40}, subprocess.CalledProcessError(128, "git"))):
            output, _ = self.run_event(name, event, result)
            self.assertEqual(output, "backend=true\nfrontend=true\nworker=true\n")

    def test_pull_request_uses_merge_base_and_keeps_renames_visible(self):
        event = {"pull_request": {"base": {"sha": "base"}, "head": {"sha": "head"}}}
        output, args = self.run_event("pull_request", event, lambda *a, **k: b"frontend/src/main.ts\0")
        self.assertEqual(output, "backend=false\nfrontend=true\nworker=false\n")
        self.assertIn("base...head", args.args[0])
        self.assertIn("--no-renames", args.args[0])
