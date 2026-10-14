import copy
import hashlib
from pathlib import Path
import tempfile
import unittest

from service_manifest import validate_service


def elf(machine=62, e_type=3):
    header = bytearray(64)
    header[0:4] = b"\x7fELF"
    header[4], header[5], header[6] = 2, 1, 1
    header[16:18] = e_type.to_bytes(2, "little")
    header[18:20] = machine.to_bytes(2, "little")
    return bytes(header)


class ServiceManifestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "service").mkdir()
        self.binary = elf()
        (self.root / "service/example-echo").write_bytes(self.binary)
        self.manifest = {
            "id": "example-echo",
            "resources": {"service/example-echo": hashlib.sha256(self.binary).hexdigest()},
            "service": {
                "binary": "service/example-echo",
                "kernelApi": {"major": 0, "minMinor": 5},
                "capabilities": ["log"],
                "fetchHosts": ["pool.example.test"],
                "memoryMax": "64M",
                "shutdownSeconds": 30,
            },
        }

    def check(self, **changes):
        manifest = copy.deepcopy(self.manifest)
        for key, value in changes.items():
            if value is None:
                manifest["service"].pop(key, None)
            else:
                manifest["service"][key] = value
        return validate_service(manifest, self.root)

    def test_valid_section_is_normalized(self):
        value = validate_service(self.manifest, self.root)
        self.assertEqual(value["name"], "example-echo")
        self.assertEqual(value["memoryMaxMiB"], 64)
        self.assertEqual(value["capabilities"], ["log"])

    def test_manifest_without_service_section_is_not_a_service(self):
        self.assertIsNone(validate_service({"resources": {}}, self.root))

    def test_rejects_invalid_binary_paths(self):
        for binary in ["service/../example-echo", "service/Upper", "bin/example-echo", "/service/x", "service/a.b"]:
            with self.subTest(binary=binary):
                with self.assertRaises(ValueError):
                    self.check(binary=binary)

    def test_binary_must_be_listed_as_resource(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["resources"] = {}
        with self.assertRaises(ValueError):
            validate_service(manifest, self.root)

    def test_rejects_non_elf_and_wrong_machine_or_type(self):
        for content in [b"#!/bin/sh\n" + b"x" * 64, elf(machine=183), elf(e_type=1)]:
            with self.subTest(content=content[:8]):
                (self.root / "service/example-echo").write_bytes(content)
                with self.assertRaisesRegex(ValueError, "ELF"):
                    validate_service(self.manifest, self.root)
        (self.root / "service/example-echo").write_bytes(self.binary)

    def test_rejects_out_of_range_numbers(self):
        cases = [{"memoryMax": "8M"}, {"memoryMax": "513M"}, {"memoryMax": "64"}, {"memoryMax": 64},
                 {"shutdownSeconds": 4}, {"shutdownSeconds": 601}, {"shutdownSeconds": True},
                 {"shutdownSeconds": "30"}]
        for changes in cases:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.check(**changes)

    def test_accepts_boundary_values(self):
        self.assertEqual(self.check(memoryMax="16M", shutdownSeconds=5)["memoryMaxMiB"], 16)
        self.assertEqual(self.check(memoryMax="512M", shutdownSeconds=600)["shutdownSeconds"], 600)
        for minor in (4, 5, 6):
            self.assertEqual(self.check(kernelApi={"major": 0, "minMinor": minor})["kernelApi"]["minMinor"], minor)

    def test_rejects_unknown_capabilities_and_duplicates(self):
        for value in [["exec"], ["log", "log"], ["log", 1], "log"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.check(capabilities=value)

    def test_rejects_invalid_hosts(self):
        for value in [["Pool.example.test"], ["a_b.test"], ["-bad.test"], ["x" * 64 + ".test"], "pool.example.test"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.check(fetchHosts=value)

    def test_rejects_incompatible_kernel_api(self):
        for value in [{"major": 1, "minMinor": 5}, {"major": 0, "minMinor": 3}, {"major": 0, "minMinor": 7},
                      {"major": 0}, {"major": 0, "minMinor": 5, "extra": 1}]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.check(kernelApi=value)

    def test_rejects_missing_or_unknown_fields(self):
        for key in ["binary", "kernelApi", "capabilities", "fetchHosts", "memoryMax", "shutdownSeconds"]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.check(**{key: None})
        with self.assertRaises(ValueError):
            self.check(runAsRoot=True)


if __name__ == "__main__":
    unittest.main()
