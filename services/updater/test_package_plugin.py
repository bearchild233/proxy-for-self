import hashlib
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

from package_plugin import package_plugin


class PackageTests(unittest.TestCase):
    def test_excel_package_contains_frontend_manifest_and_license(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "plugin/src/excel_codex_bridge"
            source.mkdir(parents=True)
            (source / "hub_worker.py").write_text("# worker")
            ui = root / "plugin/ui"
            ui.mkdir()
            (ui / "overview.html").write_text("<h1>Plugin</h1>")
            (root / "plugin/LICENSE").write_text("license fixture")
            runtime = root / "runtime"
            (runtime / "venv").mkdir(parents=True)
            output = root / "plugin.tar.gz"
            result = package_plugin(runtime, source, output, "1.0.0")
            self.assertEqual(result["sha256"], hashlib.sha256(output.read_bytes()).hexdigest())
            with tarfile.open(output) as archive:
                self.assertIn("LICENSE", archive.getnames())
                self.assertIn("ui/overview.html", archive.getnames())
                manifest = json.load(archive.extractfile("plugin.json"))
                self.assertEqual(manifest["capabilities"], ["excel-inference"])
                self.assertEqual(manifest["operations"], [{"id": "status", "roles": ["admin"]}])
