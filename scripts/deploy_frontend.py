"""Publish built static files without restarting services; switch index.html last."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import tarfile
import tempfile
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--destination', type=Path, required=True)
    parser.add_argument('--backup-root', type=Path, required=True)
    parser.add_argument('--verify-url', required=True)
    args = parser.parse_args()
    if hashlib.sha256(args.archive.read_bytes()).hexdigest() != args.sha256:
        raise SystemExit('Archive checksum mismatch')
    destination = args.destination.resolve(strict=True)
    if not (destination / 'index.html').is_file():
        raise SystemExit('Destination is not an existing frontend')
    stamp = datetime.now(timezone.utc).strftime('%Y%m%d-%H%M%S') + '-' + secrets.token_hex(3)
    backup = args.backup_root.resolve() / ('frontend-' + stamp)
    backup.mkdir(parents=True, mode=0o700)
    old_index = (destination / 'index.html').read_bytes()
    with tempfile.TemporaryDirectory(prefix='frontend-') as temp:
        stage = Path(temp)
        with tarfile.open(args.archive, 'r:gz') as archive:
            members = archive.getmembers()
            if len(members) > 10000 or sum(item.size for item in members) > 100 * 1024 * 1024:
                raise SystemExit('Archive exceeds static bundle limits')
            for member in members:
                path = Path(member.name)
                if not member.isfile() or path.is_absolute() or '..' in path.parts or '\\' in member.name:
                    raise SystemExit('Invalid archive entry')
                target = stage / path
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.extractfile(member) as source:
                    target.write_bytes(source.read())
        if not (stage / 'index.html').is_file():
            raise SystemExit('Missing entry point')
        shutil.copytree(destination, backup / 'frontend', symlinks=True)
        # 保留旧 hash 资源，避免已打开页面的懒加载模块失效。
        def publish(source, target):
            target.parent.mkdir(parents=True, exist_ok=True)
            if not target.parent.resolve().is_relative_to(destination) or target.is_symlink():
                raise RuntimeError('Unsafe destination path')
            temporary = target.with_name('.stage-' + secrets.token_hex(6))
            try:
                temporary.write_bytes(source)
                temporary.chmod(0o644)
                os.replace(temporary, target)
            finally:
                temporary.unlink(missing_ok=True)

        for source in sorted(stage.rglob('*')):
            if not source.is_file() or source == stage / 'index.html':
                continue
            target = destination / source.relative_to(stage)
            if target.exists() and target.read_bytes() == source.read_bytes():
                continue
            if target.exists() and source.relative_to(stage).parts[0] == 'assets':
                raise RuntimeError('Existing asset name has different content')
            publish(source.read_bytes(), target)
        try:
            publish((stage / 'index.html').read_bytes(), destination / 'index.html')
            with urllib.request.urlopen(args.verify_url, timeout=15) as response:
                if response.read() != (stage / 'index.html').read_bytes():
                    raise RuntimeError('Served index does not match the new bundle')
        except Exception:
            publish(old_index, destination / 'index.html')
            raise
    receipt = {'sha256': args.sha256, 'destination': str(destination), 'backup': str(backup),
               'deployed_at': stamp, 'service_restart': False}
    (backup / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt))


if __name__ == '__main__':
    main()
