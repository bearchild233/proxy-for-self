"""Start the local plugin staging gateway in the build WSL distro only.

Requires the local test PostgreSQL at /work/test-services. No production URLs
or credentials are accepted. The HTTP shell runs separately on Windows.
"""
import json
import argparse
import signal
import os
from pathlib import Path
import platform
import pwd
import secrets
import socket
import subprocess
import time
from urllib.request import urlopen


def main():
    if 'microsoft' not in platform.release().lower() or os.geteuid() != 0:
        raise SystemExit('Run as root in the local build WSL distro')
    parser=argparse.ArgumentParser();parser.add_argument('--restart',action='store_true');args=parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    base = Path('/work/plugin-stage')
    user = pwd.getpwnam('builder')
    base.mkdir(mode=0o700, exist_ok=True)
    os.chown(base, user.pw_uid, user.pw_gid)
    os.chmod(base, 0o700)

    def private(path, text):
        path.write_text(text, encoding='utf-8')
        os.chmod(path, 0o600)
        os.chown(path, user.pw_uid, user.pw_gid)

    credentials = json.loads(Path('/work/test-services/credentials.json').read_text())
    secret_path = base / 'credentials.json'
    if not secret_path.exists():
        private(secret_path, json.dumps({'redis': secrets.token_hex(24), 'admin': secrets.token_hex(24)}))
    secret = json.loads(secret_path.read_text())
    env = {key: value for key, value in os.environ.items() if not key.startswith('CPR_')}
    env.update(CPR_PLUGIN_POLICY_RUNNER=str(repo/'services/plugin_host/policy_runner.py'), CPR_PLUGIN_ROOT=str(repo/'.build/plugin-platform'))
    env.update(PGPASSFILE='/work/test-services/pgpass', REDISCLI_AUTH=secret['redis'], LANG='C.UTF-8')

    def run(args):
        result = subprocess.run(['runuser', '-u', 'builder', '--', *args], env=env, capture_output=True, text=True)
        if result.returncode:
            # Never include secret-bearing configuration or connection arguments.
            raise RuntimeError(Path(args[0]).name + ' failed; inspect local staging logs')
        return result.stdout.strip()

    pgbin = '/usr/lib/postgresql/18/bin/'
    pg = ['-h', '127.0.0.1', '-p', '15432', '-U', 'cprtest']
    if run([pgbin + 'psql', *pg, '-d', 'postgres', '-Atc', "select 1 from pg_database where datname='cpr_plugin_stage'"]) != '1':
        run([pgbin + 'createdb', *pg, 'cpr_plugin_stage'])

    def port_free(port):
        with socket.socket() as sock:
            return sock.connect_ex(('127.0.0.1', port)) != 0

    redis_config = base / 'redis.conf'
    private(redis_config, '\n'.join([
        'bind 127.0.0.1', 'port 16380', 'protected-mode yes',
        'requirepass ' + secret['redis'], 'daemonize yes',
        'pidfile ' + str(base / 'redis.pid'), 'logfile ' + str(base / 'redis.log'),
        'dir ' + str(base), 'save ""', 'appendonly no',
        'maxmemory 32mb', 'maxmemory-policy noeviction', '',
    ]))
    if port_free(16380):
        run(['redis-server', str(redis_config)])
    if run(['redis-cli', '-h', '127.0.0.1', '-p', '16380', 'ping']) != 'PONG':
        raise RuntimeError('Staging Redis is unavailable')

    for name in ['deploy', 'data', 'logs', 'web']:
        path = base / name
        path.mkdir(exist_ok=True)
        os.chown(path, user.pw_uid, user.pw_gid)
    vault = base / 'data/vault-key.hex'
    if not vault.exists():
        private(vault, secrets.token_hex(32))
    config = {
        'schema_version': 1,
        'host': {'listen': {'host': '127.0.0.1', 'port': 18336}, 'runtime_data_dir': str(base / 'data'),
                 'logging': {'level': 'info', 'stdout': True, 'oauth_recovery': False, 'request_dump': False,
                             'file': {'enabled': True, 'directory': str(base / 'logs'), 'retention_days': 2, 'max_file_size_mb': 5}},
                 'drain_timeout_seconds': 5, 'worker_shutdown_timeout_seconds': 5},
        'store': {'vault_key_file': str(vault),
                  'database': {'url': 'postgres://cprtest@127.0.0.1:15432/cpr_plugin_stage', 'password': credentials['postgres']},
                  'redis': {'url': 'redis://127.0.0.1:16380/', 'password': secret['redis']},
                  'pool': {'max_connections': 4, 'acquire_timeout_seconds': 5}},
        'admin': {'session_ttl_minutes': 1440, 'default_username': 'stage@local.test', 'default_password': secret['admin']},
        'client': {'session_ttl_minutes': 1440},
        'api': {'trusted_proxy_ips': [], 'asset_directory': str(base / 'web'), 'cors_allowed_origins': [],
                'request_timeout_seconds': None, 'request_id_header': 'x-request-id',
                'inference_limits': {'max_requests': 2, 'max_body_bytes': 1048576, 'max_in_flight_body_bytes': 2097152}},
        'openai': {'pinned_cli_auto_update': False, 'excel_worker_socket': str(base/'excel/worker.sock'), 'excel_plugin_state_file':str(base/'excel/state.json')},
    }
    # JSON is valid YAML and avoids a dependency on developer-installed PyYAML.
    private(base / 'deploy/config.yaml', json.dumps(config))
    access = repo / '.build/plugin-stage/access.json'
    access.parent.mkdir(parents=True, exist_ok=True)
    access.write_text(json.dumps({'username': 'stage@local.test', 'password': secret['admin']}))
    binary = Path('/work/proxy-for-self/backend/target/debug/codex-proxy-rs')
    if args.restart and not port_free(18336):
        old_pid=int((base/'gateway.pid').read_text())
        if Path(f'/proc/{old_pid}/cwd').resolve()!=base or str(Path(f'/proc/{old_pid}/exe').resolve()).removesuffix(' (deleted)')!=str(binary):
            raise RuntimeError('Refusing to stop a process outside the isolated stage')
        os.kill(old_pid,signal.SIGTERM)
        for _ in range(100):
            if port_free(18336):break
            time.sleep(.1)
        if not port_free(18336):raise RuntimeError('Local gateway still draining')
    if port_free(18336):
        with (base / 'gateway.log').open('ab') as log:
            process = subprocess.Popen([str(binary)], cwd=base, env=env, stdin=subprocess.DEVNULL,
                                       stdout=log, stderr=log, start_new_session=True,
                                       user=user.pw_uid, group=user.pw_gid, extra_groups=[])
        private(base / 'gateway.pid', str(process.pid))
    pid = int((base / 'gateway.pid').read_text())
    if Path(f'/proc/{pid}/cwd').resolve() != base or Path(f'/proc/{pid}/exe').resolve() != binary:
        raise RuntimeError('Port is not owned by this staging gateway')
    for _ in range(30):
        try:
            with urlopen('http://127.0.0.1:18336/healthz', timeout=1) as response:
                if response.status in {200, 204}:
                    print(json.dumps({'gateway_pid': pid, 'gateway_port': 18336,
                                      'database': 'cpr_plugin_stage', 'redis_port': 16380, 'production': False}))
                    return
        except OSError:
            time.sleep(0.5)
    raise RuntimeError('Gateway did not become ready; inspect /work/plugin-stage/gateway.log')


if __name__ == '__main__':
    main()
