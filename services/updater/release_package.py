"""Build a managed release: gateway + frontend, with explicit database compatibility."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import tarfile


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"\d+\.\d+\.\d+", args.version):
        parser.error("a stable version such as 0.1.0 is required")
    root = Path(__file__).resolve().parents[2]
    files = {"bin/api-hub": args.binary}
    for path in sorted((root / "frontend/dist").rglob("*")):
        if path.is_file():
            files["frontend/" + path.relative_to(root / "frontend/dist").as_posix()] = path
    if "frontend/index.html" not in files:
        parser.error("frontend/dist/index.html is required")
    manifest = {
        "format": 1, "application": "proxy-for-self", "version": args.version,
        "platform": "linux-amd64", "plugin_protocols": {"excel": 1},
        "migrations": {str(int(p.name.split("_", 1)[0])): hashlib.sha384(p.read_bytes()).hexdigest()
                       for p in sorted((root / "backend/migrations").glob("*.sql"))},
        "files": {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in files.items()},
    }
    args.output.mkdir(parents=True, exist_ok=True)
    manifest_path = args.output / "release-manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    archive = args.output / f"proxy-for-self-{args.version}-linux-amd64.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        tar.add(manifest_path, arcname="release-manifest.json", recursive=False)
        for name, path in files.items():
            tar.add(path, arcname=name, recursive=False)
    (args.output / "checksums.txt").write_text(
        hashlib.sha256(archive.read_bytes()).hexdigest() + "  " + archive.name + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
