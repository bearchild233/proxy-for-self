"""Local WSL Excel lifecycle adapter. Uses the production installer with local processes."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import platform
import pwd
import signal
import subprocess
import sys
import time
from types import SimpleNamespace

REPO=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(REPO/'services/updater'))
from plugins import PluginManager
from package_plugin import package_plugin

BASE=Path('/work/plugin-stage/excel')


def stop():
    path=BASE/'worker.pid'
    if not path.exists(): return
    pid=int(path.read_text())
    proc=Path(f'/proc/{pid}')
    if not proc.exists(): return
    if (proc/'cwd').resolve() != BASE:
        raise ValueError('Local worker PID ownership mismatch')
    os.kill(pid,signal.SIGTERM)
    for _ in range(6300):
        if not proc.exists() or (proc/'stat').read_text().split()[2]=='Z':return
        time.sleep(.1)
    raise ValueError('Local worker did not drain')


def run(command, **_):
    if command[:2]==['systemctl','show']:
        return SimpleNamespace(stdout='on-failure' if 'Restart' in command else '')
    if command[:2]==['systemctl','stop']:
        stop()
    if command[:3]==['systemctl','enable','--now']:
        socket=BASE/'worker.sock';socket.unlink(missing_ok=True)
        env={'PATH':'/usr/bin:/bin','LANG':'C.UTF-8','EXCEL_PLUGIN_STATE_FILE':str(BASE/'state.json')}
        with (BASE/'worker.log').open('ab') as log:
            process=subprocess.Popen([str(BASE/'runtime/venv/bin/python'),'-I','-m','excel_codex_bridge.hub_worker','--socket',str(socket),'--max-concurrency','2','--idle-timeout','0'],cwd=BASE,env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=log,start_new_session=True,user=pwd.getpwnam('builder').pw_uid,group=pwd.getpwnam('builder').pw_gid,extra_groups=[])
        (BASE/'worker.pid').write_text(str(process.pid))
    return SimpleNamespace(stdout='')


def main():
    if 'microsoft' not in platform.release().lower():raise SystemExit('Local WSL only')
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('action',choices=['prepare','status','install','enable','disable','update','uninstall']);args=parser.parse_args()
    BASE.mkdir(parents=True,exist_ok=True,mode=0o700)
    owner=pwd.getpwnam('builder');os.chown(BASE,owner.pw_uid,owner.pw_gid)
    config_file=BASE/'installer.json'
    if args.action=='prepare':
        runtime=BASE/'build';runtime.mkdir(exist_ok=True)
        python=runtime/'venv/bin/python'
        if not python.exists():subprocess.run([sys.executable,'-I','-m','venv',str(runtime/'venv')],check=True)
        project=REPO/'plugins/excel-bridge'
        subprocess.run([str(python),'-I','-m','pip','--isolated','install','--no-deps','-r',str(project/'requirements.lock')],check=True,stdout=sys.stderr)
        subprocess.run([str(python),'-I','-m','pip','check'],check=True,stdout=sys.stderr)
        package=package_plugin(runtime,project/'src/excel_codex_bridge',BASE/'package.tar.gz','0.2.0')
        config_file.write_text(json.dumps({**package,'root':str(BASE),'socket':str(BASE/'worker.sock'),'drain_seconds':90}))
        print(json.dumps({'prepared':True}));return
    if not config_file.exists():
        print(json.dumps({'id':'excel-bridge','installed':False,'enabled':False,'operation':{'status':'idle','message':'本机 Worker 尚未准备'}}));return
    if args.action=='status':
        state=json.loads((BASE/'state.json').read_text()) if (BASE/'state.json').exists() else {}
        print(json.dumps({'id':'excel-bridge','installed':(BASE/'runtime').is_dir(),'enabled':bool(state.get('enabled')),'version':state.get('version'),'operation':state.get('operation',{'status':'idle'})}));return
    with (BASE/'control.lock').open('a+b') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        manager=PluginManager(json.loads(config_file.read_text()),run)
        manager.begin('excel-bridge',args.action)
        manager.execute(args.action)
        print(json.dumps(manager.status(),ensure_ascii=False))


if __name__=='__main__':main()
