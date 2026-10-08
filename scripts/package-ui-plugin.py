"""Package one already-built UI plugin. Does not compile or deploy the gateway."""
import argparse
import hashlib
from pathlib import Path
import re
import sys

REPO=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(REPO/'services/plugin_host'))
from server import package_directory

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('plugin')
args=parser.parse_args()
if not re.fullmatch(r'[a-z][a-z0-9-]{0,63}',args.plugin):parser.error('Invalid plugin ID')
source=REPO/'.build/plugins'/args.plugin/'package'
if not (source/'plugin.json').is_file():parser.error('Build this plugin first')
output=REPO/'.build/plugin-releases';output.mkdir(exist_ok=True)
archive=output/(args.plugin+'.tar.gz')
package_directory(source,archive)
digest=hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix('.gz.sha256').write_text(digest+'  '+archive.name+'\n')
print(archive)
