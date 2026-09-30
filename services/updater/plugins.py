"""受信任插件注册表与独立生命周期；客户端不能传路径、命令或下载地址。"""
from __future__ import annotations

import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import socket
import tarfile
import tempfile
import time
import uuid


class PluginManager:
    def __init__(self, config, run):
        self.config = config
        self.run = run
        self.root = Path(config["root"]).resolve(strict=True)
        self.state_path = self.root / "state.json"
        self.state = json.loads(self.state_path.read_text())
        if self.state.get("operation", {}).get("status") == "running":
            # 中断后不猜测安装结果；禁用入口，允许重新安装/禁用/卸载进行恢复。
            self.state.update(enabled=False)
            self.state["operation"].update(status="failed", message="上次插件操作中断；请重新安装或卸载恢复")
            self.save()

    def save(self):
        temporary = self.root / (".state-" + uuid.uuid4().hex)
        try:
            with temporary.open("x") as stream:
                json.dump(self.state, stream, ensure_ascii=False)
                stream.flush()
                os.fsync(stream.fileno())
            temporary.chmod(0o644)
            os.replace(temporary, self.state_path)
        finally:
            temporary.unlink(missing_ok=True)

    def status(self):
        runtime = self.root / "runtime"
        installed = runtime.is_dir() and not runtime.is_symlink()
        return {"id": "excel-bridge", "name": "Excel Bridge", "protocol": 1,
                "installed": installed, "enabled": installed and self.state.get("enabled") is True,
                "version": self.state.get("version") if installed else None,
                "availableVersion": self.config["version"], "operation": self.state.get("operation", {"status": "idle"})}

    def begin(self, plugin_id, action):
        if plugin_id != "excel-bridge" or action not in {"install", "enable", "disable", "uninstall", "update"}:
            raise ValueError("未知插件或操作")
        if self.state.get("operation", {}).get("status") == "running":
            raise ValueError("插件操作正在执行")
        if action in {"enable", "update"} and not self.status()["installed"]:
            raise ValueError("插件尚未安装")
        self.ensure_independent()
        operation_id = "plugin-" + uuid.uuid4().hex
        self.state["operation"] = {"id": operation_id, "action": action, "status": "running", "message": "操作已受理"}
        # 先关闭准入，再排空。不会停止已有流，也不碰网关主进程。
        self.state["enabled"] = False
        self.save()
        return operation_id

    def ensure_independent(self):
        # Requires 等依赖在显式停止时传播，Restart=on-failure 无法兜底。
        # 检查 systemd 实际生效值，不能只检查文件文本或以 drop-in 清空依赖。
        dependencies = self.run([
            "systemctl", "show", "api-hub.service", "--value",
            "-p", "Requires", "-p", "Requisite", "-p", "BindsTo", "-p", "PartOf",
        ]).stdout.split()
        if {"api-hub-excel.socket", "api-hub-excel.service"}.intersection(dependencies):
            raise ValueError("主网关仍依赖 Excel 服务，已拒绝操作；请先解除 systemd 强依赖")

    def remove_runtime(self):
        path = self.root / "runtime"
        if path.is_symlink() or path.resolve().parent != self.root:
            raise ValueError("插件运行目录无效")
        if path.exists():
            shutil.rmtree(path)

    def check_health(self):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            connection = http.client.HTTPConnection("localhost", timeout=2)
            try:
                connection.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                connection.sock.settimeout(2)
                connection.sock.connect("/run/api-hub-excel/worker.sock")
                connection.request("GET", "/healthz")
                response = connection.getresponse()
                if response.status == 200 and json.loads(response.read(1024)).get("ok") is True:
                    return
            except (OSError, ValueError, http.client.HTTPException):
                pass
            finally:
                connection.close()
            time.sleep(0.25)
        raise ValueError("Excel Worker 健康检查失败，插件保持禁用，请检查运行目录权限与服务日志")

    def install(self):
        archive = Path(self.config["package"])
        if archive.stat().st_size > 256 * 1024 * 1024:
            raise ValueError("插件包超过限制")
        digest = hashlib.sha256()
        with archive.open("rb") as stream:
            while chunk := stream.read(65536):
                digest.update(chunk)
        if digest.hexdigest() != self.config["sha256"]:
            raise ValueError("插件包校验失败")
        with tempfile.TemporaryDirectory(prefix=".install-", dir=self.root) as directory:
            stage = Path(directory)
            names, total = set(), 0
            with tarfile.open(archive, "r:gz") as package:
                for member in package:
                    path = PurePosixPath(member.name)
                    total += member.size
                    if (not member.isfile() or path.is_absolute() or ".." in path.parts
                            or "\\" in member.name or str(path) != member.name or member.name in names
                            or total > 512 * 1024 * 1024 or len(names) >= 15000):
                        raise ValueError("插件包包含非法路径或超过限制")
                    names.add(member.name)
                    output = stage / member.name
                    output.parent.mkdir(parents=True, exist_ok=True)
                    with package.extractfile(member) as source, output.open("xb") as target:
                        shutil.copyfileobj(source, target, 65536)
                    output.chmod(0o644)
            manifest = json.loads((stage / "plugin.json").read_text())
            if (manifest.get("id") != "excel-bridge" or manifest.get("protocol") != 1
                    or manifest.get("version") != self.config["version"]
                    or not (stage / "venv/lib/python3.10/site-packages/excel_codex_bridge/hub_worker.py").is_file()):
                raise ValueError("插件协议或版本不兼容")
            # Python 链接由受信任安装器生成，不接受压缩包链接。
            bindir = stage / "venv/bin"
            bindir.mkdir(parents=True, exist_ok=True)
            for name in ("python", "python3", "python3.10"):
                (bindir / name).unlink(missing_ok=True)
                (bindir / name).symlink_to("/usr/bin/python3")
            for directory, _, _ in os.walk(stage):
                Path(directory).chmod(0o755)
            self.run([str(bindir / "python"), "-B", "-c", "import excel_codex_bridge.hub_worker"], timeout=30)
            current = self.root / "runtime"
            if current.is_symlink() or current.resolve().parent != self.root:
                raise ValueError("插件运行目录无效")
            previous = self.root / (".previous-" + uuid.uuid4().hex)
            if current.exists():
                os.replace(current, previous)
            try:
                os.replace(stage, current)
            except OSError:
                if previous.exists():
                    os.replace(previous, current)
                raise
            if previous.exists():
                shutil.rmtree(previous)
        self.state["version"] = self.config["version"]

    def execute(self, action):
        try:
            with (self.root / "activity.lock").open("rb") as lock:
                deadline = time.monotonic() + self.config.get("drain_seconds", 620)
                self.state["operation"]["message"] = "等待正在运行的 Excel 请求完成"
                self.save()
                while True:
                    try:
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        break
                    except BlockingIOError:
                        if time.monotonic() >= deadline:
                            raise ValueError("仍有 Excel 请求运行；已关闭新请求，请稍后重试")
                        time.sleep(0.2)
                self.ensure_independent()
                self.run(["systemctl", "disable", "--now", "api-hub-excel.socket"], timeout=60)
                self.run(["systemctl", "stop", "api-hub-excel.service"], timeout=650)
                if action in {"install", "update"}:
                    self.install()
                elif action == "uninstall":
                    self.remove_runtime()
                    self.state["version"] = None
                if action in {"enable", "install", "update"}:
                    manifest = json.loads((self.root / "runtime/plugin.json").read_text())
                    if manifest.get("id") != "excel-bridge" or manifest.get("protocol") != 1:
                        raise ValueError("已安装插件协议不兼容，请重新安装")
                    self.state["version"] = manifest["version"]
                    # 先确保开机 socket 启动及 worker 自动重启保护，再开放准入。
                    policy = self.run(["systemctl", "show", "api-hub-excel.service", "-p", "Restart", "--value"]).stdout.strip()
                    if policy not in {"always", "on-failure"}:
                        raise ValueError("Excel 自动重启保护未启用")
                    self.run(["systemctl", "enable", "--now", "api-hub-excel.socket"])
                    self.check_health()
                    self.state["enabled"] = True
                self.state["operation"].update(status="succeeded", message="插件操作已完成")
                self.save()
        except Exception as error:
            self.state["enabled"] = False
            message = str(error) if isinstance(error, (ValueError, RuntimeError)) else "插件操作失败，请检查受信任安装包与服务配置"
            self.state["operation"].update(status="failed", message=message)
            self.save()
