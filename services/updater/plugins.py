"""受信任插件注册表与独立生命周期；客户端不能传路径、命令或下载地址。"""
from __future__ import annotations

import fcntl
import hashlib
import http.client
import json
import os
import re
from pathlib import Path, PurePosixPath
import shutil
import socket
import tarfile
import tempfile
import time
import uuid

from plugin_manifest import identifier, validate


class PluginManager:
    def __init__(self, config, run):
        if not isinstance(config.get("required", False), bool):
            raise ValueError("基础模块标记必须为布尔值")
        self.config = config
        self.run = run
        self.id = identifier(config.get("id", "excel-bridge"))
        self.name = config.get("name", "Excel Bridge" if self.id == "excel-bridge" else self.id)
        unit = "api-hub-excel" if self.id == "excel-bridge" else "api-hub-plugin-" + self.id
        self.service = config.get("service", unit + ".service")
        self.socket_unit = config.get("socket_unit", unit + ".socket")
        self.gateway_service = config.get("gateway_service", "api-hub.service")
        if any(not isinstance(value,str) or not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9_.@-]*[.](service|socket)',value) for value in (self.service,self.socket_unit,self.gateway_service)):
            raise ValueError("服务单元名称无效")
        self.socket_path = config.get("socket", "/run/" + unit + "/worker.sock")
        self.previous = None
        self.root = Path(config["root"]).resolve(strict=True)
        self.state_path = self.root / "state.json"
        pending = self.root / ".previous-runtime"
        self.previous = pending if pending.exists() else None
        if not self.state_path.exists():
            self.state = {"enabled": False, "operation": {"status": "idle"}}
            self.save()
        (self.root / "activity.lock").touch(exist_ok=True)
        (self.root / "activity.lock").chmod(0o644)
        self.state = json.loads(self.state_path.read_text())
        if self.state.get("operation", {}).get("status") == "running":
            # 恢复也是排空操作；完成前保持准入关闭，网关不参与重启。
            self.execute("recover")

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
        return {"id": self.id, "name": self.name, "protocol": 1,
                "required": self.config.get("required", False),
                "description": self.config.get("description", ""),
                "installed": installed, "enabled": installed and self.state.get("enabled") is True,
                "version": self.state.get("version") if installed else None,
                "availableVersion": self.config["version"], "operation": self.state.get("operation", {"status": "idle"})}

    def begin(self, plugin_id, action):
        if plugin_id != self.id or action not in {"install", "enable", "disable", "uninstall", "update"}:
            raise ValueError("未知插件或操作")
        # required 由受信任宿主注册表决定，不接受页面或插件包自行变更。
        if self.config.get("required") is True and action in {"disable", "uninstall"}:
            raise ValueError("基础模块不可停用或卸载")
        if self.state.get("operation", {}).get("status") == "running":
            raise ValueError("插件操作正在执行")
        if action in {"enable", "update"} and not self.status()["installed"]:
            raise ValueError("插件尚未安装")
        self.ensure_independent()
        operation_id = "plugin-" + uuid.uuid4().hex
        self.state["previous_enabled"] = bool(self.state.get("enabled"))
        self.state["operation"] = {"id": operation_id, "action": action, "status": "running", "message": "操作已受理"}
        # 先关闭准入，再排空。不会停止已有流，也不碰网关主进程。
        self.state["enabled"] = False
        self.save()
        return operation_id

    def ensure_independent(self):
        # Requires 等依赖在显式停止时传播，Restart=on-failure 无法兜底。
        # 检查 systemd 实际生效值，不能只检查文件文本或以 drop-in 清空依赖。
        dependencies = self.run([
            "systemctl", "show", self.gateway_service, "--value",
            "-p", "Requires", "-p", "Requisite", "-p", "BindsTo", "-p", "PartOf",
        ]).stdout.split()
        if {self.socket_unit, self.service}.intersection(dependencies):
            raise ValueError("主网关仍依赖插件服务，已拒绝操作；请先解除 systemd 强依赖")

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
                connection.sock.connect(self.socket_path)
                connection.request("GET", "/healthz")
                response = connection.getresponse()
                if response.status == 200 and json.loads(response.read(1024)).get("ok") is True:
                    return
            except (OSError, ValueError, http.client.HTTPException):
                pass
            finally:
                connection.close()
            time.sleep(0.25)
        raise ValueError("插件健康检查失败，插件保持禁用，请检查运行目录权限与服务日志")

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
            validate(manifest, self.id, stage, self.config["version"])
            # 解释器链接由受信任安装器生成，插件包不能执行 root 安装脚本。
            if self.id == "excel-bridge" or self.config.get("runtime") == "python3.10":
                bindir = stage / "venv/bin"
                bindir.mkdir(parents=True, exist_ok=True)
                for name in ("python", "python3", "python3.10"):
                    (bindir / name).unlink(missing_ok=True)
                    (bindir / name).symlink_to("/usr/bin/python3")
            for directory, _, _ in os.walk(stage):
                Path(directory).chmod(0o755)
            current = self.root / "runtime"
            if current.is_symlink() or current.resolve().parent != self.root:
                raise ValueError("插件运行目录无效")
            previous = self.root / ".previous-runtime"
            if previous.exists() or previous.is_symlink():
                raise ValueError("旧插件仍待恢复，请先禁用或卸载恢复")
            if current.exists():
                os.replace(current, previous)
            try:
                os.replace(stage, current)
            except OSError:
                if previous.exists():
                    os.replace(previous, current)
                raise
            self.previous = previous if previous.exists() else None
        self.state["version"] = self.config["version"]

    def execute(self, action):
        activated = False
        stopped = False
        previous_enabled = bool(self.state.get("previous_enabled", False))
        try:
            with (self.root / "activity.lock").open("rb") as lock:
                deadline = time.monotonic() + self.config.get("drain_seconds", 620)
                self.state["operation"]["message"] = "等待正在运行的插件请求完成"
                self.save()
                while True:
                    try:
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        break
                    except BlockingIOError:
                        if time.monotonic() >= deadline:
                            raise ValueError("仍有插件请求运行；已关闭新请求，请稍后重试")
                        time.sleep(0.2)
                self.ensure_independent()
                stopped = True
                self.run(["systemctl", "disable", "--now", self.socket_unit], timeout=60)
                self.run(["systemctl", "stop", self.service], timeout=650)
                if self.previous is not None:
                    self.rollback_runtime()
                if action in {"install", "update"}:
                    self.install()
                elif action == "uninstall":
                    self.remove_runtime()
                    self.state["version"] = None
                if action in {"enable", "install"} or (action in {"update", "recover"} and previous_enabled):
                    manifest = json.loads((self.root / "runtime/plugin.json").read_text())
                    validate(manifest, self.id, self.root / "runtime")
                    self.state["version"] = manifest["version"]
                    # 先确保开机 socket 启动及 worker 自动重启保护，再开放准入。
                    policy = self.run(["systemctl", "show", self.service, "-p", "Restart", "--value"]).stdout.strip()
                    if policy not in {"always", "on-failure"}:
                        raise ValueError("插件自动重启保护未启用")
                    activated = True
                    self.run(["systemctl", "enable", "--now", self.socket_unit])
                    self.check_health()
                    self.state["enabled"] = True
                if self.previous is not None:
                    shutil.rmtree(self.previous)
                    self.previous = None
                self.state["operation"].update(status="succeeded", message="插件操作已完成")
                self.save()
        except Exception as error:
            # 更新失败恢复旧版本及原启用状态；排空超时不停止正在执行的 Worker。
            rollback_failed = False
            try:
                if activated or self.previous is not None:
                    self.run(["systemctl", "disable", "--now", self.socket_unit], timeout=60)
                    self.run(["systemctl", "stop", self.service], timeout=60)
                if self.previous is not None:
                    self.rollback_runtime()
                if previous_enabled and stopped and self.status()["installed"]:
                    self.run(["systemctl", "enable", "--now", self.socket_unit])
                    self.check_health()
            except Exception:
                rollback_failed = True
            self.state["enabled"] = previous_enabled and not rollback_failed and self.status()["installed"]
            message = str(error) if isinstance(error, (ValueError, RuntimeError)) else "插件操作失败，请检查受信任安装包与服务配置"
            if rollback_failed:
                message = "插件操作失败，旧运行目录待恢复；请重试禁用或卸载"
            self.state["operation"].update(status="failed", message=message)
            self.save()

    def rollback_runtime(self):
        previous = self.previous
        if previous is None:
            return
        if previous.is_symlink() or previous.resolve().parent != self.root:
            raise ValueError("插件恢复目录无效")
        self.remove_runtime()
        os.replace(previous, self.root / "runtime")
        self.previous = None
        try:
            self.state["version"] = json.loads((self.root / "runtime/plugin.json").read_text())["version"]
        except (OSError, ValueError, KeyError):
            self.state["version"] = None
