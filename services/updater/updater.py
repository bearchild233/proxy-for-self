"""Independent systemd updater. Accepts only fixed operations over a private Unix socket."""
from __future__ import annotations

from datetime import datetime, timezone
import argparse
import fcntl
import grp
import hashlib
from http.server import BaseHTTPRequestHandler
import json
import os
import platform
from pathlib import Path, PurePosixPath
import re
import shutil
import socketserver
import stat
import subprocess
import tarfile
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import uuid
import sys

# -I 不自动加载脚本目录；这里只加载 root 管理的同目录模块。
sys.path.insert(0, str(Path(__file__).resolve().parent))
from plugins import PluginManager

REPOSITORY = "ckcyian23/proxy-for-self"
MAX_ARCHIVE = 512 * 1024 * 1024
MAX_EXTRACTED = 1024 * 1024 * 1024


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(65536):
            digest.update(chunk)
    return digest.hexdigest()


def version(value: str) -> tuple[int, ...]:
    if not isinstance(value, str) or not re.fullmatch(r"\d{1,5}\.\d{1,5}\.\d{1,5}", value):
        raise ValueError("仅支持明确的稳定版本号，例如 0.1.0")
    return tuple(map(int, value.split(".")))


def write_json(path: Path, value: dict) -> None:
    temporary = path.with_name(path.name + ".tmp-" + uuid.uuid4().hex)
    try:
        with temporary.open("x", encoding="utf-8") as stream:
            json.dump(value, stream, ensure_ascii=False, indent=2)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def trusted_url(url: str) -> None:
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme != "https" or parsed.username or parsed.password or parsed.port not in (None, 443)
            or parsed.hostname not in {"api.github.com", "github.com", "release-assets.githubusercontent.com", "objects.githubusercontent.com"}):
        raise ValueError("发布资源不在受信任的 GitHub HTTPS 地址内")


class TrustedRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        trusted_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download(url: str, target: Path, limit: int) -> None:
    trusted_url(url)
    accept = "application/vnd.github+json" if urllib.parse.urlsplit(url).hostname == "api.github.com" else "application/octet-stream"
    request = urllib.request.Request(url, headers={"User-Agent": "proxy-for-self-updater", "Accept": accept})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), TrustedRedirect())
    with opener.open(request, timeout=60) as response, target.open("xb") as output:
        count = 0
        while chunk := response.read(65536):
            count += len(chunk)
            if count > limit:
                raise ValueError("发布资源超过大小限制")
            output.write(chunk)


def unpack(archive: Path, destination: Path, target_version: str) -> dict:
    """不接受符号链接、硬链接、重复成员或包外路径；所有有效载荷必须逐文件校验。"""
    names = set()
    total = 0
    with tarfile.open(archive, "r:gz") as package:
        for member in package:
            path = PurePosixPath(member.name)
            allowed = member.name in {"bin/api-hub", "release-manifest.json"} or member.name.startswith("frontend/")
            if (not member.isfile() or not allowed or path.is_absolute() or ".." in path.parts
                    or "\\" in member.name or str(path) != member.name or member.name in names):
                raise ValueError("更新包含有非法或重复的文件路径")
            total += member.size
            if total > MAX_EXTRACTED or len(names) >= 10000:
                raise ValueError("更新包解压大小或文件数量超过限制")
            names.add(member.name)
            output = destination / member.name
            output.parent.mkdir(parents=True, exist_ok=True)
            with package.extractfile(member) as source, output.open("xb") as stream:
                shutil.copyfileobj(source, stream, 65536)
            output.chmod(0o755 if member.name == "bin/api-hub" else 0o644)
    manifest = json.loads((destination / "release-manifest.json").read_text())
    if (manifest.get("format") != 1 or manifest.get("application") != "proxy-for-self"
            or manifest.get("version") != target_version or manifest.get("platform") != "linux-amd64"
            or manifest.get("plugin_protocols") != {"excel": 1}):
        raise ValueError("更新包版本、平台或插件协议不兼容")
    files = manifest.get("files", {})
    if set(files) != names - {"release-manifest.json"} or not {"bin/api-hub", "frontend/index.html"} <= set(files):
        raise ValueError("更新包文件清单不完整")
    for name, expected in files.items():
        if file_hash(destination / name) != expected:
            raise ValueError("更新包内部文件校验失败")
    return manifest


class Updater:
    def __init__(self, config: dict):
        self.config = config
        self.root = Path(config["release_root"]).resolve(strict=True)
        self.state_path = Path(config["state_file"])
        self.state_path.parent.mkdir(parents=True, exist_ok=True)
        self.lock = threading.Lock()
        self.state = json.loads(self.state_path.read_text()) if self.state_path.exists() else {
            "previous_version": None, "current_version": config["initial_version"], "need_restart": False,
            "operation": {"status": "idle"},
        }
        version(self.state["current_version"])
        if self.state["operation"]["status"] == "running":
            # 执行者异常退出后禁止自动重复写入；已切换版本由人工核对状态再恢复。
            self.state["operation"].update(status="failed", error="更新服务曾中断，请检查当前版本及备份后再操作", finished_at=now())
            self.state["recovery_required"] = True
            write_json(self.state_path, self.state)

        self.plugins = PluginManager(config["plugins"], self.run) if config.get("plugins") else None

    def plugin_status(self):
        with self.lock:
            return [self.plugins.status()] if self.plugins else []

    def plugin_submit(self, plugin_id, action):
        with self.lock:
            if not self.plugins or self.state.get("recovery_required") or self.state["operation"]["status"] == "running":
                raise ValueError("插件管理不可用或系统操作正在执行")
            operation_id = self.plugins.begin(plugin_id, action)
        threading.Thread(target=self.plugins.execute, args=(action,), daemon=True).start()
        return {"operation_id": operation_id}

    def current(self) -> Path:
        path = (self.root / "current").resolve(strict=True)
        if path.parent != self.root / "releases" or not path.is_dir():
            raise ValueError("当前版本目录不在配置的 releases 内")
        return path

    def run(self, args: list[str], **kwargs) -> subprocess.CompletedProcess:
        result = subprocess.run(args, capture_output=True, text=True, timeout=kwargs.pop("timeout", 60), **kwargs)
        if result.returncode:
            raise RuntimeError("系统命令执行失败：" + Path(args[0]).name)
        return result

    def pg(self, name: str) -> tuple[list[str], dict]:
        runtime = self.current() / "runtime"
        db = self.config["database"]
        return ([str(runtime / "usr/lib/postgresql/18/bin" / name), "-h", db["host"], "-p", str(db["port"]), "-U", db["user"]],
                dict(os.environ, PGPASSFILE=db["pgpass_file"], LD_LIBRARY_PATH=str(runtime / "lib")))

    def sql(self, query: str) -> str:
        command, env = self.pg("psql")
        return self.run(command + ["-d", self.config["database"]["name"], "-At", "-v", "ON_ERROR_STOP=1"],
                        input="begin read only;\n" + query + ";\nrollback;", env=env).stdout.removeprefix("BEGIN\n").removesuffix("ROLLBACK\n").strip()

    def schemas(self) -> dict:
        migrations = json.loads(self.sql("select case when bool_and(success) then json_object_agg(version::text,encode(checksum,'hex')) else null end from _sqlx_migrations"))
        if not isinstance(migrations,dict) or not migrations:
            raise ValueError("数据库存在失败或缺失的迁移，停止操作")
        return migrations

    def status(self) -> dict:
        with self.lock:
            return json.loads(json.dumps(self.state))

    def persist(self, **changes) -> None:
        with self.lock:
            self.state["operation"].update(changes)
            write_json(self.state_path, self.state)

    def submit(self, action: str, target: str | None) -> dict:
        if action not in {"update", "rollback", "restart"}:
            raise ValueError("不支持的更新操作")
        with self.lock:
            if self.state.get("recovery_required"):
                raise ValueError("上次更新中断，需要维护者检查后恢复")
            if self.state["operation"]["status"] == "running" or (self.plugins and self.plugins.state.get("operation", {}).get("status") == "running"):
                raise ValueError("已有系统操作正在执行")
            if action == "update":
                current = version(self.state["current_version"])
                desired = version(target)
                if desired <= current or desired[0] != current[0]:
                    raise ValueError("只允许当前主版本内向前升级")
            elif action == "rollback" and not self.state.get("previous_release"):
                raise ValueError("没有可用的上一版本")
            operation_id = "managed-" + uuid.uuid4().hex
            self.state["operation"] = {"operation_id":operation_id, "kind":action, "status":"running",
                "target_version":target, "message":"操作已受理", "error":None, "started_at":now(), "finished_at":None}
            write_json(self.state_path, self.state)
        threading.Thread(target=self.execute, args=(action, target), daemon=True).start()
        return {"operation_id":operation_id}

    def backup(self) -> Path:
        backup = Path(self.config["backup_root"]) / (datetime.now().strftime("%Y%m%d-%H%M%S-") + uuid.uuid4().hex[:8])
        backup.mkdir(parents=True, mode=0o700)
        shutil.copytree(self.config["config_directory"], backup / "config", symlinks=True)
        command, env = self.pg("pg_dump")
        self.run(command + ["-d",self.config["database"]["name"], "-Fc", "-f",str(backup / "database.dump")], env=env, timeout=180)
        restore, env = self.pg("pg_restore")
        # --list 不连接数据库；验证归档目录可读。
        self.run([restore[0], "--list", str(backup / "database.dump")], env=env)
        write_json(backup / "deployment.json", {"current":str(self.current()), "state":self.status(), "migrations":self.schemas()})
        return backup

    def prepare(self, target: str, temporary: Path) -> tuple[Path, dict]:
        metadata = temporary / "github.json"
        download(f"https://api.github.com/repos/{REPOSITORY}/releases/tags/v{target}", metadata, 2*1024*1024)
        release = json.loads(metadata.read_text())
        if release.get("draft") or release.get("prerelease") or release.get("tag_name") != "v" + target:
            raise ValueError("目标不是正式发布版本")
        assets = {entry["name"]: entry for entry in release["assets"]}
        filename = f"proxy-for-self-{target}-linux-amd64.tar.gz"
        checksum_file = temporary / "checksums.txt"
        for name, path, limit in [("checksums.txt",checksum_file,65536), (filename,temporary / filename,MAX_ARCHIVE)]:
            asset = assets[name]
            expected_url = f"https://github.com/{REPOSITORY}/releases/download/v{target}/{name}"
            if asset["browser_download_url"] != expected_url or not 0 < asset["size"] <= limit:
                raise ValueError("发布资源地址或大小无效")
            download(expected_url, path, limit)
        entries = [line.split() for line in checksum_file.read_text().splitlines()]
        checksums = [parts[0] for parts in entries if len(parts)==2 and parts[1].lstrip("*")==filename]
        if len(checksums)!=1 or file_hash(temporary / filename)!=checksums[0]:
            raise ValueError("发布包 SHA256 校验失败")
        unpacked = temporary / "unpacked"
        unpacked.mkdir()
        manifest = unpack(temporary / filename, unpacked, target)
        if manifest["migrations"] != self.schemas():
            raise ValueError("该版本包含数据库迁移，需要维护升级；当前服务未变更")
        destination = self.root / "releases" / ("release-" + target + "-" + checksums[0][:12])
        if destination.exists():
            raise ValueError("目标目录已经存在，请维护者核对上次升级记录")
        shutil.copytree(self.current(), destination, copy_function=os.link, symlinks=True)
        for name in [*manifest["files"], "release-manifest.json"]:
            output = destination / name
            output.parent.mkdir(parents=True, exist_ok=True)
            if not output.parent.resolve().is_relative_to(destination):
                raise ValueError("已有版本中的目录链接越出 release 边界")
            staged = output.with_name(output.name + ".new-" + uuid.uuid4().hex)
            shutil.copyfile(unpacked / name, staged)
            staged.chmod(0o755 if name == "bin/api-hub" else 0o644)
            os.replace(staged, output)
        for directory, _, _ in os.walk(destination / "frontend"):
            Path(directory).chmod(0o755)
        destination.chmod(0o755)
        write_json(destination / "managed-release.json", {"version":target, "migrations":manifest["migrations"]})
        return destination, manifest

    def activate(self, target: Path) -> None:
        if target.resolve().parent != self.root / "releases":
            raise ValueError("无效的版本目录")
        link = self.root / (".current-" + uuid.uuid4().hex)
        link.symlink_to(target)
        os.replace(link, self.root / "current")

    def healthy(self) -> bool:
        try:
            with urllib.request.urlopen(self.config["health_url"], timeout=2) as response:
                return response.status in (200,204)
        except (OSError, ValueError):
            return False

    def restart(self) -> None:
        service = self.config["service"]
        self.ensure_restart_protection()
        # 独立 unit 持有操作，网关退出不会杀死执行者。systemd 负责原有请求的优雅排空。
        self.run(["systemctl","restart",service], timeout=720)
        deadline = time.monotonic()+40
        while time.monotonic()<deadline:
            if self.healthy():
                return
            time.sleep(1)
        raise RuntimeError("新进程健康检查未通过")

    def ensure_restart_protection(self) -> None:
        service = self.config["service"]
        policy = self.run(["systemctl","show",service,"-p","Restart","--value"]).stdout.strip()
        if policy not in {"always", "on-failure"}:
            raise ValueError("服务自动重启保护未启用，停止操作")

    def execute(self, action: str, target: str | None) -> None:
        previous = None
        desired = None
        switched = False
        try:
            previous = self.current()
            self.ensure_restart_protection()
            if shutil.disk_usage(self.root).free < 1200 * 1024 * 1024 and action != "restart":
                raise ValueError("可用磁盘空间不足 1.2 GiB，停止更新")
            self.persist(message="检查并备份当前版本")
            with tempfile.TemporaryDirectory(prefix="update-", dir=self.state_path.parent) as directory:
                desired = previous
                if action == "update":
                    self.persist(message="下载并校验正式发布包")
                    desired, _ = self.prepare(target, Path(directory))
                elif action == "rollback":
                    desired = Path(self.state["previous_release"])
                    if self.state.get("previous_migrations") != self.schemas():
                        raise ValueError("数据库版本已变化，不能自动回滚程序")
                    target = self.state["previous_version"]
                if action != "restart":
                    backup = self.backup()
                    self.persist(message="备份完成，等待正在执行的请求结束", backup=str(backup))
                deadline = time.monotonic()+180
                while int(self.sql("select count(*) from model_requests where outcome='running' and deadline_at>now()")):
                    if time.monotonic() >= deadline:
                        raise ValueError("请求持续繁忙，本次未切换版本，请稍后重试")
                    time.sleep(2)
                self.persist(message="正在优雅重启，请稍候恢复连接")
                if action != "restart":
                    self.activate(desired)
                    switched = True
                self.restart()
                with self.lock:
                    if action != "restart":
                        self.state.update(previous_release=str(previous), previous_version=self.state["current_version"],
                            previous_migrations=self.schemas(), current_version=target, need_restart=False)
                self.persist(status="succeeded", message="服务已恢复，更新已生效" if action!="restart" else "服务已重新启动", finished_at=now())
        except Exception as error:
            message = str(error) if isinstance(error,(ValueError,RuntimeError)) else "系统更新失败，请维护者检查执行日志"
            if switched and previous is not None:
                try:
                    self.run(["systemctl","stop",self.config["service"]], timeout=720)
                    self.activate(previous)
                    self.restart()
                    message += "；已恢复上一版本"
                except Exception:
                    with self.lock:
                        self.state["recovery_required"] = True
                    message += "；自动恢复未完成，需要维护者处理"
            elif action == "update" and desired is not None and desired != previous:
                # 仅清理本次创建且从未激活的目标，避免繁忙超时后无法重试。
                if desired.resolve().parent == self.root / "releases" and desired != self.current():
                    shutil.rmtree(desired)
            self.persist(status="failed", message=message, error=message, finished_at=now())


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(5)

    def do_GET(self):
        if self.path == "/plugins":
            self.reply(200, self.server.updater.plugin_status())
        elif self.path == "/status":
            self.reply(200, self.server.updater.status())
        else:
            self.reply(404, {})

    def do_POST(self):
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if self.path not in {"/operation", "/plugins/action"} or not 0 < length <= 1024:
                raise ValueError("无效请求")
            value = json.loads(self.rfile.read(length))
            if self.path == "/plugins/action":
                if not isinstance(value, dict) or set(value) != {"id", "action"}:
                    raise ValueError("无效插件操作字段")
                self.reply(202, self.server.updater.plugin_submit(value["id"], value["action"]))
                return
            if not isinstance(value,dict) or set(value) - {"action","target_version"}:
                raise ValueError("无效操作字段")
            self.reply(202, self.server.updater.submit(value.get("action"), value.get("target_version")))
        except (ValueError, TypeError):
            self.reply(409, {"error":"操作无效、已有任务执行中，或需要维护者恢复状态"})

    def reply(self, code, value):
        body = json.dumps(value, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header("Content-Type","application/json")
        self.send_header("Content-Length",str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", type=Path, required=True)
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() not in {"x86_64", "amd64"}:
        raise SystemExit("managed updater currently supports Linux amd64 only")
    os.umask(0o077)
    metadata = args.config.stat()
    if metadata.st_uid != 0 or metadata.st_mode & 0o022:
        raise SystemExit("updater config must be root-owned and not writable by other users")
    config = json.loads(args.config.read_text())
    if not re.fullmatch(r"[a-zA-Z0-9_.@-]+\.service",config["service"]):
        raise SystemExit("invalid service unit")
    # 单个 updater 进程；重启前遗留的 socket 只在拿到独占锁后移除。
    lock_path = Path(config["state_file"]).with_suffix(".lock")
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        updater = Updater(config)
        socket = Path(config["socket"])
        socket.parent.mkdir(parents=True, exist_ok=True)
        if socket.exists():
            if not stat.S_ISSOCK(socket.lstat().st_mode):
                raise SystemExit("control socket path is not a socket")
            socket.unlink()
        with Server(str(socket), Handler) as server:
            server.updater = updater
            os.chown(socket, 0, grp.getgrnam(config["socket_group"]).gr_gid)
            socket.chmod(0o660)
            server.serve_forever()


if __name__ == "__main__":
    main()
