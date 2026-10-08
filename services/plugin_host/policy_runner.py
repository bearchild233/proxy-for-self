"""Gateway-owned policy adapter: data-only input, no credentials or database access."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import uuid
from contextlib import contextmanager

@contextmanager
def state_lock(root):
    directory = Path(root) / 'pricing-runtime'
    directory.mkdir(exist_ok=True)
    with (directory / '.lock').open('a+b') as handle:
        if sys.platform == 'win32':
            import msvcrt
            handle.write(b'0');handle.flush();handle.seek(0)
            msvcrt.locking(handle.fileno(), msvcrt.LK_LOCK, 1)
        else:
            import fcntl
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try: yield
        finally:
            if sys.platform == 'win32':
                handle.seek(0);msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
            else: fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def decide(root, data, plugin='backup'):
    if plugin not in {'backup', 'pricing'}: raise ValueError('unsupported plugin')
    if plugin == 'pricing':
        with state_lock(root):
            path = Path(root) / 'pricing-runtime' / 'status.json'
            previous = json.loads(path.read_text(encoding='utf-8')) if path.exists() else {}
            payload = {**data, 'state': previous}
            if data['action'] == 'claim': payload['runId'] = uuid.uuid4().hex
            result = run_policy(root, payload, plugin)
            if 'state' in result:
                temporary = path.with_suffix('.tmp')
                temporary.write_text(json.dumps(result.pop('state'), ensure_ascii=False), encoding='utf-8')
                temporary.chmod(0o640)
                temporary.replace(path)
            return result
    return run_policy(root, data, plugin)


def run_policy(root, data, plugin):
    directory = Path(root) / 'plugins' / plugin
    state = json.loads((directory / 'active.json').read_text(encoding='utf-8'))
    if not state.get('enabled'):
        return {'enabled': False}
    version = state.get('current', '')
    if len(version) != 64 or any(c not in '0123456789abcdef' for c in version):
        raise ValueError('invalid version')
    release = directory / 'releases' / version
    manifest = json.loads((release / 'plugin.json').read_text(encoding='utf-8'))
    entry = manifest.get('policyWorker')
    if entry != 'worker/policy.py' or manifest.get('sdk') != 1:
        raise ValueError('unsupported policy')
    worker = release / entry
    if worker.is_symlink() or hashlib.sha256(worker.read_bytes()).hexdigest() != manifest['resources'][entry]:
        raise ValueError('invalid policy digest')
    result = subprocess.run([sys.executable, '-I', str(worker)], input=json.dumps(data),
                            capture_output=True, text=True, encoding='utf-8', timeout=3,
                            env={'LANG':'C.UTF-8'}, cwd=release)
    if result.returncode or len(result.stdout) > 131072:
        raise ValueError('policy failed')
    return json.loads(result.stdout)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--plugin', choices=['backup', 'pricing'], default='backup')
    args = parser.parse_args()
    try:
        payload = sys.stdin.read(262145)
        if len(payload) > 262144:raise ValueError('input too large')
        print(json.dumps(decide(args.root, json.loads(payload), args.plugin)))
    except Exception:
        print('{"enabled":false}')
