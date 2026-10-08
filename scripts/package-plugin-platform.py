"""Package the control plane, shell and bootstrap modules without private runtime data."""
import argparse
import hashlib
import json
from pathlib import Path
import tarfile

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
files = {}
for directory, pattern in [('services/plugin_host','*.py'),('services/updater','*.py'),
                           ('packages/plugin-sdk','*.json'),('frontend/dist','**/*'),
                           ('deploy/plugin-platform','*'),('deploy/ab-slots','*')]:
    for path in (ROOT/directory).glob(pattern):
        if path.is_file() and not path.name.startswith('test_'):
            files[path.relative_to(ROOT).as_posix()] = path
policies=json.loads((ROOT/'packages/plugin-sdk/policies.json').read_text(encoding='utf-8'))
for plugin in policies:
    directory=ROOT/'.build/plugins'/plugin/'package'
    if not (directory/'plugin.json').is_file():raise SystemExit('Missing built plugin: '+plugin)
    for path in directory.rglob('*'):
        if path.is_file():files[path.relative_to(ROOT).as_posix()] = path
if 'frontend/dist/index.html' not in files:raise SystemExit('Build shell first')
files['LICENSE']=ROOT/'LICENSE'
args.output.mkdir(parents=True,exist_ok=True)
archive=args.output/'proxy-for-self-plugin-platform.tar.gz'
with tarfile.open(archive,'w:gz') as package:
    for name,path in sorted(files.items()):package.add(path,arcname=name,recursive=False)
archive.with_suffix('.gz.sha256').write_text(hashlib.sha256(archive.read_bytes()).hexdigest()+'  '+archive.name+'\n')
print(json.dumps({'archive':str(archive),'files':len(files),'protocol':2}))
