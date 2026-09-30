import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from updater import Updater, unpack, trusted_url, version, write_json


class PackageTests(unittest.TestCase):
    def package(self, root, *, extra=None, checksum=None, protocol=1):
        files = {"bin/api-hub": b"binary", "frontend/index.html": b"index"}
        manifest = {"format":1, "application":"proxy-for-self", "version":"0.1.1", "platform":"linux-amd64",
                    "plugin_protocols":{"excel":protocol}, "migrations":{"22":"checksum"},
                    "files":{name:hashlib.sha256(data).hexdigest() for name,data in files.items()}}
        if checksum:
            manifest["files"]["bin/api-hub"] = checksum
        files["release-manifest.json"] = json.dumps(manifest).encode()
        archive = root / "release.tar.gz"
        with tarfile.open(archive,"w:gz") as tar:
            for name,data in files.items():
                member = tarfile.TarInfo(name)
                member.size = len(data)
                tar.addfile(member,io.BytesIO(data))
            if extra:
                tar.addfile(extra,io.BytesIO(b""))
        return archive

    def test_valid_release_preserves_only_verified_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destination = root / "unpacked"
            destination.mkdir()
            result = unpack(self.package(root),destination,"0.1.1")
            self.assertEqual(result["version"],"0.1.1")
            self.assertEqual((destination / "bin/api-hub").stat().st_mode & 0o777,0o755)

    def test_rejects_paths_links_and_duplicate_members(self):
        for name, kind in [("../escape",tarfile.REGTYPE),("frontend/../../escape",tarfile.REGTYPE),
                           ("frontend/link",tarfile.SYMTYPE),("frontend/link",tarfile.LNKTYPE),
                           ("bin/api-hub",tarfile.REGTYPE)]:
            with self.subTest(name=name,kind=kind), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                member = tarfile.TarInfo(name)
                member.type = kind
                member.linkname = "/etc/passwd" if kind != tarfile.REGTYPE else ""
                destination = root / "unpacked"
                destination.mkdir()
                with self.assertRaises(ValueError):
                    unpack(self.package(root,extra=member),destination,"0.1.1")

    def test_rejects_corruption_and_incompatible_plugin_protocol(self):
        for options in [{"checksum":"0"*64},{"protocol":2}]:
            with self.subTest(options=options), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                destination = root / "unpacked"
                destination.mkdir()
                with self.assertRaises(ValueError):
                    unpack(self.package(root,**options),destination,"0.1.1")

    def test_download_and_version_boundaries(self):
        trusted_url("https://github.com/ckcyian23/proxy-for-self/releases/download/v0.1.1/release.tar.gz")
        for url in ["http://github.com/file","https://github.com.evil.test/file","https://user:password@github.com/file","https://github.com:444/file"]:
            with self.subTest(url=url),self.assertRaises(ValueError):
                trusted_url(url)
        for value in ["../1.0.0","1.0.0;reboot","1.0.0-beta",None]:
            with self.subTest(value=value),self.assertRaises(ValueError):
                version(value)


class TransactionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.previous = self.root / "releases/old"
        self.desired = self.root / "releases/new"
        self.previous.mkdir(parents=True)
        self.desired.mkdir()
        (self.root / "current").symlink_to(self.previous)
        self.config = {"release_root":str(self.root),"state_file":str(self.root / "state.json"),
                       "initial_version":"0.1.0","service":"fixture.service"}
        self.updater = Updater(self.config)
        self.updater.state["operation"] = {"status":"running"}

    def execute(self):
        with patch.object(self.updater,"prepare",return_value=(self.desired,{})), \
             patch.object(self.updater,"ensure_restart_protection"), \
             patch.object(self.updater,"backup",return_value=self.root / "backup"), \
             patch.object(self.updater,"sql",return_value="0"), \
             patch.object(self.updater,"schemas",return_value={"22":"checksum"}), \
             patch("updater.shutil.disk_usage") as disk:
            disk.return_value.free = 4*1024**3
            self.updater.execute("update","0.1.1")

    def test_success_requires_completed_restart_and_health(self):
        with patch.object(self.updater,"restart") as restart:
            self.execute()
        restart.assert_called_once()
        self.assertEqual((self.root / "current").resolve(),self.desired)
        self.assertEqual(self.updater.status()["current_version"],"0.1.1")
        self.assertEqual(self.updater.status()["operation"]["status"],"succeeded")

    def test_failed_health_rolls_back_and_reports_failure(self):
        with patch.object(self.updater,"restart",side_effect=[RuntimeError("health failed"),None]) as restart, \
             patch.object(self.updater,"run"):
            self.execute()
        self.assertEqual(restart.call_count,2)
        self.assertEqual((self.root / "current").resolve(),self.previous)
        self.assertEqual(self.updater.status()["current_version"],"0.1.0")
        self.assertIn("已恢复上一版本",self.updater.status()["operation"]["error"])

    def test_database_incompatibility_does_not_switch_or_restart(self):
        archive = PackageTests().package(self.root)
        filename = "proxy-for-self-0.1.1-linux-amd64.tar.gz"
        payload = archive.read_bytes()
        checksum = hashlib.sha256(payload).hexdigest() + "  " + filename + "\n"
        release = {"tag_name":"v0.1.1", "draft":False, "prerelease":False, "assets":[
            {"name":name,"size":len(data),"browser_download_url":"https://github.com/ckcyian23/proxy-for-self/releases/download/v0.1.1/"+name}
            for name,data in [(filename,payload),("checksums.txt",checksum.encode())]
        ]}
        def download(url, path, limit):
            data = json.dumps(release).encode() if "api.github.com" in url else checksum.encode() if url.endswith("checksums.txt") else payload
            path.write_bytes(data)
        temporary = self.root / "staging"
        temporary.mkdir()
        with patch("updater.download",side_effect=download), patch.object(self.updater,"schemas",return_value={"22":"different-checksum"}):
            with self.assertRaisesRegex(ValueError,"数据库迁移"):
                self.updater.prepare("0.1.1",temporary)
        self.assertEqual((self.root / "current").resolve(),self.previous)
        self.assertEqual(set((self.root / "releases").iterdir()),{self.previous,self.desired})

    def test_interrupted_executor_refuses_new_mutations(self):
        write_json(Path(self.config["state_file"]),self.updater.state)
        restarted = Updater(self.config)
        self.assertTrue(restarted.status()["recovery_required"])
        with self.assertRaises(ValueError):
            restarted.submit("restart",None)

    def test_restart_protection_cannot_be_disabled(self):
        import subprocess
        with patch.object(self.updater,"run",return_value=subprocess.CompletedProcess([],0,"no\n")) as run:
            with self.assertRaises(ValueError):
                self.updater.restart()
        self.assertEqual(run.call_count,1)

    def test_plugin_and_system_operations_are_mutually_exclusive(self):
        from types import SimpleNamespace
        self.updater.plugins = SimpleNamespace(state={"operation": {"status": "running"}})
        self.updater.state["operation"]["status"] = "idle"
        with self.assertRaises(ValueError):
            self.updater.submit("restart", None)
        self.updater.plugins.state["operation"]["status"] = "idle"
        self.updater.state["operation"]["status"] = "running"
        with self.assertRaises(ValueError):
            self.updater.plugin_submit("excel-bridge", "disable")

    def test_failed_or_missing_database_migrations_stop_updates(self):
        for value in ["null","{}"]:
            with self.subTest(value=value),patch.object(self.updater,"sql",return_value=value),self.assertRaises(ValueError):
                self.updater.schemas()


if __name__ == "__main__":
    unittest.main()
