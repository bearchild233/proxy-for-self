"""服务插件监督（root 运行的 updater）：运行目录、env 与 drop-in、systemd 实例、内核注册表。

安全边界：
- 插件用户只能写插件存储目录（读取版本包）、自己的 socket 目录与数据目录；
- 执行的二进制由 updater 从存储复制到 root 所有的运行目录，复制后再次校验哈希与 ELF 头，
  因此插件用户替换存储中的文件不会影响即将执行的内容；
- 能力与 fetchHosts 的上限只来自 updater 配置中的 allow，清单申请值与之取交集后写入注册表。

所有 systemd 与外部命令通过注入的 run 执行，单元测试不需要真实 systemd。
"""
from __future__ import annotations

from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import threading
import time
import uuid

import kernel_protocol
from service_manifest import KERNEL_CAPABILITIES, is_x86_64_elf, validate_service

PLUGIN_ID = re.compile(r"[a-z][a-z0-9-]{0,63}\Z")
DIGEST = re.compile(r"[0-9a-f]{64}\Z")
ACTIONS = frozenset({"activate", "enable", "disable", "rollback", "uninstall"})
UNIT_TEMPLATE = "proxy-for-self-plugin@{instance}.service"
REQUIRED_CONFIG = ("registry_file", "store_root", "runtime_root", "socket_root", "data_root",
                   "env_directory", "state_file", "user", "group", "allow")
# 任务给出的数值：新实例须在该时限内通过健康检查；切换后等待内核重新加载注册表（两倍加载周期）；
# 停止旧实例时 systemd 的 TimeoutStopSec 为其 shutdownSeconds 加上 STOP_MARGIN_SECONDS。
HEALTH_DEADLINE_SECONDS = 15
RELOAD_WAIT_SECONDS = 10
STOP_MARGIN_SECONDS = 5
# 状态中缺少某版本的关停超时记录时（只会出现在被手工改动的状态文件里），按清单允许的最大值等待，避免提前强制停止。
SHUTDOWN_FALLBACK_SECONDS = 600
# 006 推断、007 经设计负责人接受的数值（任务未给出，但已确认沿用）。
PROBE_TIMEOUT_SECONDS = 2  # 单次健康探测的连接与读取超时，超时后按间隔重试直到健康期限
PROBE_INTERVAL_SECONDS = 0.25  # 健康探测失败后再次探测前的等待间隔
SYSTEMCTL_TIMEOUT_SECONDS = 60  # daemon-reload、start、is-active 等 systemctl 调用的执行超时
STOP_CALL_SLACK_SECONDS = 10  # systemctl stop 的外层执行超时 = shutdownSeconds + STOP_MARGIN_SECONDS + 本余量，避免外层先于 systemd 超时
MAX_MANIFEST_BYTES = 65536  # 与控制面 static_plugins 的 plugin.json 上限保持一致
MAX_RESOURCE_BYTES = 4 * 1024 * 1024  # 与控制面 static_plugins 的单文件上限保持一致
DEFAULT_UNIT_DIRECTORY = "/etc/systemd/system"


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def instance_name(plugin_id: str, version: str) -> str:
    # 实例名为 <插件ID>--<内容版本前 12 位>，新旧版本可以并存。
    return f"{plugin_id}--{version[:12]}"


def system_identity(user: str, group: str) -> tuple[int, int]:
    import grp
    import pwd
    return pwd.getpwnam(user).pw_uid, grp.getgrnam(group).gr_gid


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(65536):
            digest.update(chunk)
    return digest.hexdigest()


def _write_atomic(path: Path, text: str, mode: int) -> None:
    temporary = path.with_name(path.name + ".tmp-" + uuid.uuid4().hex)
    try:
        with temporary.open("x", encoding="utf-8") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def _ensure_dir(path: Path, mode: int, owner: tuple[int, int] | None = None) -> None:
    path.mkdir(parents=True, exist_ok=True)
    if path.is_symlink() or not path.is_dir():
        raise ValueError("插件目录无效")
    # 先改所有者再设权限：chown 可能清除 setgid 位。
    if owner is not None:
        os.chown(path, owner[0], owner[1])
    os.chmod(path, mode)


def _safe_relative(name) -> str:
    if not isinstance(name, str):
        raise ValueError("资源路径无效")
    path = PurePosixPath(name)
    if not name or path.is_absolute() or ".." in path.parts or "\\" in name or str(path) != name:
        raise ValueError("资源路径无效")
    return name


def _checked_config(config: dict) -> dict:
    missing = [key for key in REQUIRED_CONFIG if key not in config]
    if missing:
        raise ValueError("服务插件配置缺少字段：" + ",".join(missing))
    for key in ("registry_file", "store_root", "runtime_root", "socket_root", "data_root", "env_directory", "state_file"):
        if not isinstance(config[key], str) or not Path(config[key]).is_absolute():
            raise ValueError("服务插件路径必须为绝对路径：" + key)
    allow = config["allow"]
    if not isinstance(allow, dict):
        raise ValueError("服务插件 allow 配置无效")
    for plugin_id, limits in allow.items():
        if not PLUGIN_ID.fullmatch(plugin_id) or not isinstance(limits, dict) or set(limits) != {"grants", "fetchHosts"}:
            raise ValueError("服务插件 allow 条目无效")
        if not isinstance(limits["grants"], list) or not all(item in KERNEL_CAPABILITIES for item in limits["grants"]):
            raise ValueError("服务插件 allow 能力上限无效")
        if not isinstance(limits["fetchHosts"], list) or not all(isinstance(item, str) for item in limits["fetchHosts"]):
            raise ValueError("服务插件 allow 主机上限无效")
    checked = dict(config)
    checked.setdefault("unit_directory", DEFAULT_UNIT_DIRECTORY)
    return checked


class ServicePlugins:
    def __init__(self, config: dict, run, *, busy=None, health=None, identity=None, sleep=time.sleep, clock=time.monotonic):
        self.config = _checked_config(config)
        self.run = run
        self.busy = busy or (lambda: False)
        self.health = health or kernel_protocol.health
        self.identity = identity or system_identity
        self.sleep = sleep
        self.clock = clock
        self.lock = threading.RLock()
        self.state_path = Path(self.config["state_file"])
        self.state_path.parent.mkdir(parents=True, exist_ok=True)
        self.state = (json.loads(self.state_path.read_text(encoding="utf-8")) if self.state_path.exists()
                      else {"plugins": {}, "operation": None, "recovery_required": False})
        operation = self.state.get("operation")
        if operation and operation.get("status") == "running":
            # 执行者异常退出后不自动重放：标记中断，禁止后续服务操作，等待维护者核对。
            operation.update(status="interrupted", message="updater 重启时服务插件操作中断，请核对运行目录、注册表与 systemd 状态",
                             finished_at=now())
            self.state["recovery_required"] = True
            self._save()

    # ----- 状态与操作互斥 -----

    def _save(self) -> None:
        _write_atomic(self.state_path, json.dumps(self.state, ensure_ascii=False, indent=2), 0o600)

    @property
    def running(self) -> bool:
        operation = self.state.get("operation")
        return bool(operation) and operation.get("status") == "running"

    def _entry(self, plugin_id: str) -> dict:
        return self.state["plugins"].setdefault(plugin_id, {
            "current": None, "previous": None, "enabled": False, "registration": None, "timeouts": {}, "health": None,
        })

    def _message(self, text: str) -> None:
        with self.lock:
            if self.state.get("operation"):
                self.state["operation"]["message"] = text
                self._save()

    def begin(self, action: str, plugin_id: str, version: str | None = None) -> str:
        """受理一个服务插件操作并记录为运行中；与其他系统或插件操作互斥。"""
        if action not in ACTIONS or not isinstance(plugin_id, str) or not PLUGIN_ID.fullmatch(plugin_id):
            raise ValueError("未知服务插件操作")
        if version is not None and not (isinstance(version, str) and DIGEST.fullmatch(version)):
            raise ValueError("版本标识无效")
        if action == "activate" and version is None:
            raise ValueError("激活需要指定版本")
        with self.lock:
            if self.state.get("recovery_required"):
                raise ValueError("上次服务插件操作中断，需要维护者核对后处理")
            if self.running or self.busy():
                raise ValueError("已有系统或插件操作正在执行")
            operation_id = "service-" + uuid.uuid4().hex
            self.state["operation"] = {"id": operation_id, "plugin": plugin_id, "action": action, "version": version,
                                       "status": "running", "message": "操作已受理", "error": None,
                                       "started_at": now(), "finished_at": None}
            self._save()
            return operation_id

    def execute(self, operation_id: str, plugin_id: str, action: str, version: str | None) -> None:
        try:
            self._dispatch(plugin_id, action, version)
            self._finish(operation_id, "succeeded", "服务插件操作已完成", None)
        except Exception as error:
            message = str(error) if isinstance(error, (ValueError, RuntimeError)) else "服务插件操作失败，请检查 updater 日志"
            self._finish(operation_id, "failed", message, message)

    def _finish(self, operation_id: str, status: str, message: str, error: str | None) -> None:
        with self.lock:
            operation = self.state.get("operation") or {}
            if operation.get("id") != operation_id:
                return
            operation.update(status=status, message=message, error=error, finished_at=now())
            self._save()

    def _dispatch(self, plugin_id: str, action: str, version: str | None) -> None:
        if action == "activate":
            self.activate(plugin_id, version)
        elif action == "enable":
            self.enable(plugin_id, version)
        elif action == "disable":
            self.disable(plugin_id)
        elif action == "rollback":
            self.rollback(plugin_id)
        else:
            self.uninstall(plugin_id)

    # ----- 包与权限 -----

    def _package(self, plugin_id: str, version: str) -> tuple[dict, dict, Path]:
        """从插件存储读取版本包，重新校验清单、资源哈希与服务段。"""
        if not DIGEST.fullmatch(version):
            raise ValueError("版本标识无效")
        root = Path(self.config["store_root"]) / plugin_id / "releases" / version
        if root.is_symlink() or not root.is_dir():
            raise ValueError("插件版本不存在")
        manifest_path = root / "plugin.json"
        if manifest_path.is_symlink() or not manifest_path.is_file() or manifest_path.stat().st_size > MAX_MANIFEST_BYTES:
            raise ValueError("插件清单缺失或过大")
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        if (not isinstance(manifest, dict) or manifest.get("id") != plugin_id or manifest.get("protocol") != 2
                or manifest.get("kind") != "ui"):
            raise ValueError("插件协议不兼容")
        resources = manifest.get("resources")
        if not isinstance(resources, dict) or not resources:
            raise ValueError("资源清单无效")
        declared = {"plugin.json"}
        for name, digest in resources.items():
            _safe_relative(name)
            item = root / name
            if item.is_symlink() or not item.is_file() or item.stat().st_size > MAX_RESOURCE_BYTES:
                raise ValueError("资源缺失或过大")
            if not isinstance(digest, str) or _sha256(item) != digest:
                raise ValueError("资源校验失败")
            declared.add(name)
        for item in root.rglob("*"):
            if item.is_symlink():
                raise ValueError("版本目录包含符号链接")
            if item.is_file() and item.relative_to(root).as_posix() not in declared:
                raise ValueError("版本目录包含未声明文件")
        service = validate_service(manifest, root)
        if service is None:
            raise ValueError("插件没有服务段，不能由服务监督管理")
        return manifest, service, root

    def _registration(self, plugin_id: str, instance: str, manifest: dict, service: dict) -> dict:
        allowed = self.config["allow"].get(plugin_id)
        if allowed is None:
            raise ValueError("插件未登记服务能力上限，不能启用服务")
        uid, _ = self.identity(self.config["user"], self.config["group"])
        return {
            "socket": str(Path(self.config["socket_root"]) / plugin_id / f"{instance}.sock"),
            "uid": uid,
            # 清单申请值与 allow 上限取交集；未在上限内的能力不写入注册表。
            "grants": [item for item in service["capabilities"] if item in allowed["grants"]],
            "fetchHosts": [item for item in service["fetchHosts"] if item in allowed["fetchHosts"]],
        }

    # ----- 运行目录、env、drop-in、systemd -----

    def _runtime_binary(self, instance: str, service: dict) -> Path:
        return Path(self.config["runtime_root"]) / instance / service["name"]

    def _stage(self, instance: str, root: Path, manifest: dict, service: dict) -> None:
        """复制二进制到 root 所有的运行目录，并校验复制结果的哈希与 ELF 头。"""
        runtime_root = Path(self.config["runtime_root"])
        _ensure_dir(runtime_root, 0o755)
        runtime = runtime_root / instance
        if runtime.is_symlink():
            raise ValueError("运行目录无效")
        if runtime.exists():
            shutil.rmtree(runtime)
        runtime.mkdir(mode=0o755)
        os.chmod(runtime, 0o755)
        target = runtime / service["name"]
        shutil.copyfile(root / service["binary"], target)
        os.chmod(target, 0o755)
        if _sha256(target) != manifest["resources"][service["binary"]] or not is_x86_64_elf(target):
            raise ValueError("服务二进制复制后校验失败")

    def _verify_staged(self, instance: str, manifest: dict, service: dict) -> None:
        target = self._runtime_binary(instance, service)
        if target.is_symlink() or not target.is_file() or _sha256(target) != manifest["resources"][service["binary"]]:
            raise ValueError("已有运行目录校验失败，请先卸载后重新安装")

    def _start(self, plugin_id: str, instance: str, service: dict, registration: dict) -> None:
        uid, gid = self.identity(self.config["user"], self.config["group"])
        _ensure_dir(Path(self.config["socket_root"]), 0o755)
        # socket 目录用单独的连接组（缺省为插件组）并设 setgid，插件创建的 socket 继承该组；
        # 网关只需加入连接组即可连接，不因此获得插件存储目录的读权限。
        socket_gid = self.identity(self.config["user"], self.config.get("socket_group", self.config["group"]))[1]
        _ensure_dir(Path(self.config["socket_root"]) / plugin_id, 0o2750, (uid, socket_gid))
        _ensure_dir(Path(self.config["data_root"]), 0o755)
        data = Path(self.config["data_root"]) / plugin_id
        _ensure_dir(data, 0o700, (uid, gid))
        _ensure_dir(Path(self.config["env_directory"]), 0o755)
        environment = {
            "PLUGIN_BINARY": str(self._runtime_binary(instance, service)),
            "CPR_PLUGIN_SOCKET": registration["socket"],
            "CPR_PLUGIN_DATA": str(data),
            "CPR_PLUGIN_SHUTDOWN_MS": str(service["shutdownSeconds"] * 1000),
        }
        for key, value in environment.items():
            if any(char.isspace() for char in value):
                raise ValueError("服务路径不能包含空白字符")
        _write_atomic(Path(self.config["env_directory"]) / f"{instance}.env",
                      "".join(f"{key}={value}\n" for key, value in environment.items()), 0o644)
        # MemoryMax 与 TimeoutStopSec 不能由 EnvironmentFile 提供，使用同名实例的 drop-in。
        dropin = self._dropin(instance)
        _ensure_dir(dropin, 0o755)
        _write_atomic(dropin / "limits.conf",
                      f"[Service]\nMemoryMax={service['memoryMaxMiB']}M\n"
                      f"TimeoutStopSec={service['shutdownSeconds'] + STOP_MARGIN_SECONDS}\n", 0o644)
        self.run(["systemctl", "daemon-reload"], timeout=SYSTEMCTL_TIMEOUT_SECONDS)
        self.run(["systemctl", "start", UNIT_TEMPLATE.format(instance=instance)], timeout=SYSTEMCTL_TIMEOUT_SECONDS)

    def _dropin(self, instance: str) -> Path:
        return Path(self.config["unit_directory"]) / (UNIT_TEMPLATE.format(instance=instance) + ".d")

    def _stop(self, instance: str, shutdown_seconds: int) -> None:
        # systemd 的停止超时由 drop-in 的 TimeoutStopSec 提供；外层超时在其上加余量。
        self.run(["systemctl", "stop", UNIT_TEMPLATE.format(instance=instance)],
                 timeout=shutdown_seconds + STOP_MARGIN_SECONDS + STOP_CALL_SLACK_SECONDS)

    def _wait_healthy(self, plugin_id: str, socket_path: str) -> None:
        deadline = self.clock() + HEALTH_DEADLINE_SECONDS
        last = "未响应"
        while True:
            try:
                result = self.health(socket_path, PROBE_TIMEOUT_SECONDS, plugin_id)
                if result.get("ready") is True:
                    return
                last = "插件未就绪"
            except (OSError, kernel_protocol.RpcFailure) as error:
                last = "连接或调用失败" if not isinstance(error, kernel_protocol.RpcFailure) else error.code or "协议错误"
            if self.clock() >= deadline:
                raise RuntimeError("服务健康检查未通过：" + last)
            self.sleep(PROBE_INTERVAL_SECONDS)

    def _remove_instance(self, instance: str) -> None:
        """停止并删除一个实例的运行目录、env、drop-in 与 socket。失败时尽力继续。"""
        plugin_id = instance.rsplit("--", 1)[0]
        try:
            self.run(["systemctl", "stop", UNIT_TEMPLATE.format(instance=instance)], timeout=SYSTEMCTL_TIMEOUT_SECONDS)
        except (RuntimeError, OSError):
            pass
        runtime = Path(self.config["runtime_root"]) / instance
        if runtime.is_symlink():
            runtime.unlink()
        elif runtime.exists():
            shutil.rmtree(runtime)
        (Path(self.config["env_directory"]) / f"{instance}.env").unlink(missing_ok=True)
        dropin = self._dropin(instance)
        if dropin.is_dir() and not dropin.is_symlink():
            shutil.rmtree(dropin)
        (Path(self.config["socket_root"]) / plugin_id / f"{instance}.sock").unlink(missing_ok=True)

    def _instances_on_disk(self, plugin_id: str) -> list[str]:
        root = Path(self.config["runtime_root"])
        if not root.is_dir():
            return []
        pattern = re.compile(re.escape(plugin_id) + r"--[0-9a-f]{12}")
        return [item.name for item in root.iterdir() if item.is_dir() and pattern.fullmatch(item.name)]

    def _daemon_reload(self) -> None:
        self.run(["systemctl", "daemon-reload"], timeout=SYSTEMCTL_TIMEOUT_SECONDS)

    # ----- 注册表 -----

    def _registry_write(self, plugin_id: str, entry: dict | None) -> None:
        """原子改写内核注册表中该插件的条目；entry 为 None 时删除条目。其他条目保持不变。"""
        path = Path(self.config["registry_file"])
        value = {"plugins": []}
        if path.exists():
            value = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(value, dict) or not isinstance(value.get("plugins"), list):
                raise ValueError("内核注册表格式无效")
        plugins = [item for item in value["plugins"] if not (isinstance(item, dict) and item.get("id") == plugin_id)]
        if entry is not None:
            plugins.append({"id": plugin_id, **entry})
        value["plugins"] = plugins
        path.parent.mkdir(parents=True, exist_ok=True)
        _write_atomic(path, json.dumps(value, ensure_ascii=False, indent=2), 0o644)

    # ----- 操作 -----

    def _prune_timeouts(self, entry: dict) -> None:
        keep = {entry.get("current"), entry.get("previous")} - {None}
        entry["timeouts"] = {version: seconds for version, seconds in entry.get("timeouts", {}).items() if version in keep}

    def activate(self, plugin_id: str, version: str) -> None:
        manifest, service, root = self._package(plugin_id, version)
        instance = instance_name(plugin_id, version)
        with self.lock:
            entry = self._entry(plugin_id)
            old = entry["current"]
            if old == version and entry["enabled"]:
                return
            old_timeout = entry["timeouts"].get(old) if old else None
        registration = self._registration(plugin_id, instance, manifest, service)
        try:
            self._message("复制服务二进制并校验")
            self._stage(instance, root, manifest, service)
            self._message("启动服务实例")
            self._start(plugin_id, instance, service, registration)
            self._message("等待健康检查通过")
            self._wait_healthy(plugin_id, registration["socket"])
            self._message("切换内核注册表到新实例")
            self._registry_write(plugin_id, {**registration, "enabled": True})
        except BaseException:
            # 健康检查或注册表写入失败：删除新实例，注册表与旧实例保持不变。
            self._remove_instance(instance)
            self._daemon_reload()
            raise
        self._message("等待内核切换到新连接")
        self.sleep(RELOAD_WAIT_SECONDS)
        with self.lock:
            entry = self._entry(plugin_id)
            entry.update(current=version, previous=old, enabled=True, registration=registration, health={"ready": True, "checkedAt": now()})
            entry["timeouts"][version] = service["shutdownSeconds"]
            self._prune_timeouts(entry)
            self._save()
        if old and old != version:
            self._message("停止旧版本实例")
            self._stop(instance_name(plugin_id, old), old_timeout if old_timeout is not None else SHUTDOWN_FALLBACK_SECONDS)
        self._message("清理保留范围外的运行目录")
        self._retain(plugin_id)

    def enable(self, plugin_id: str, version: str | None = None) -> None:
        with self.lock:
            entry = self._entry(plugin_id)
            if entry["enabled"]:
                return
            version = version or entry["current"]
        if not version:
            raise ValueError("没有可启用的版本")
        manifest, service, root = self._package(plugin_id, version)
        instance = instance_name(plugin_id, version)
        registration = self._registration(plugin_id, instance, manifest, service)
        try:
            self._message("检查运行目录")
            if (Path(self.config["runtime_root"]) / instance).exists():
                # 已有运行目录时不重新复制，只校验其中的二进制。
                self._verify_staged(instance, manifest, service)
            else:
                self._stage(instance, root, manifest, service)
            self._message("启动服务实例")
            self._start(plugin_id, instance, service, registration)
            self._message("等待健康检查通过")
            self._wait_healthy(plugin_id, registration["socket"])
            self._registry_write(plugin_id, {**registration, "enabled": True})
        except BaseException:
            self._remove_instance(instance)
            self._daemon_reload()
            raise
        with self.lock:
            entry = self._entry(plugin_id)
            old = entry["current"]
            entry.update(current=version, previous=old if old != version else entry["previous"], enabled=True,
                         registration=registration, health={"ready": True, "checkedAt": now()})
            entry["timeouts"][version] = service["shutdownSeconds"]
            self._prune_timeouts(entry)
            self._save()
        self._retain(plugin_id)

    def disable(self, plugin_id: str) -> None:
        with self.lock:
            entry = self._entry(plugin_id)
            if not entry["enabled"] or not entry["current"]:
                return
            instance = instance_name(plugin_id, entry["current"])
            timeout = entry["timeouts"].get(entry["current"], SHUTDOWN_FALLBACK_SECONDS)
            registration = entry["registration"]
        if registration is not None:
            self._registry_write(plugin_id, {**registration, "enabled": False})
        self._message("等待内核停止调用")
        self.sleep(RELOAD_WAIT_SECONDS)
        self._message("停止服务实例")
        self._stop(instance, timeout)
        with self.lock:
            entry = self._entry(plugin_id)
            entry["enabled"] = False
            self._save()

    def rollback(self, plugin_id: str) -> None:
        with self.lock:
            previous = self._entry(plugin_id)["previous"]
        if not previous:
            raise ValueError("没有可回退的上一版本")
        self.activate(plugin_id, previous)

    def uninstall(self, plugin_id: str) -> None:
        self.disable(plugin_id)
        with self.lock:
            entry = self._entry(plugin_id)
            instances = {instance_name(plugin_id, version) for version in (entry["current"], entry["previous"]) if version}
        instances.update(self._instances_on_disk(plugin_id))
        self._registry_write(plugin_id, None)
        for instance in sorted(instances):
            self._remove_instance(instance)
        self._daemon_reload()
        with self.lock:
            self.state["plugins"].pop(plugin_id, None)
            self._save()

    def _retain(self, plugin_id: str) -> None:
        """只保留当前与上一版本的运行目录；更早的实例停止并删除。数据目录不在此处理。"""
        with self.lock:
            entry = self._entry(plugin_id)
            keep = {instance_name(plugin_id, version) for version in (entry["current"], entry["previous"]) if version}
            self._prune_timeouts(entry)
            self._save()
        for instance in self._instances_on_disk(plugin_id):
            if instance not in keep:
                self._remove_instance(instance)
        self._daemon_reload()

    # ----- 状态 -----

    def _unit_state(self, instance: str) -> str:
        try:
            return self.run(["systemctl", "is-active", UNIT_TEMPLATE.format(instance=instance)],
                            timeout=SYSTEMCTL_TIMEOUT_SECONDS).stdout.strip() or "unknown"
        except RuntimeError:
            # is-active 对非活动单元返回非零退出码，这是正常状态。
            return "inactive"

    def status(self) -> dict:
        with self.lock:
            snapshot = json.loads(json.dumps(self.state))
        plugins = []
        for plugin_id, entry in sorted(snapshot["plugins"].items()):
            instances = {name: instance_name(plugin_id, version) for name, version in
                         (("current", entry.get("current")), ("previous", entry.get("previous"))) if version}
            plugins.append({
                "id": plugin_id, "current": entry.get("current"), "previous": entry.get("previous"),
                "enabled": entry.get("enabled") is True, "instances": instances,
                "systemd": {name: self._unit_state(instance) for name, instance in instances.items()},
                "health": entry.get("health"),
            })
        return {"operation": snapshot.get("operation") or {"status": "idle"},
                "recoveryRequired": bool(snapshot.get("recovery_required")), "plugins": plugins}
