"""动态受信任注册表、资源读取和有界 RPC。注册文件不包含用户请求路径。"""
import fcntl
import http.client
import json
from pathlib import Path
import socket
import threading

from plugins import PluginManager
from plugin_manifest import identifier, presentation, resource, validate


class PluginRegistry:
    def __init__(self, config, run):
        self.config, self.run = config, run
        self.managers = {}
        self.lock = threading.RLock()
        self.capacity = threading.BoundedSemaphore(16)
        self.reload()

    def reload(self):
        # 可选 registry 文件可由维护者原子更新；新增插件不用重启网关或更新器。
        entries = self.config.get("entries")
        if self.config.get("registry"):
            entries = json.loads(Path(self.config["registry"]).read_text())
        if entries is None:
            entries = [dict(self.config, id="excel-bridge")]
        if not isinstance(entries, list) or len(entries) > 32:
            raise ValueError("插件注册表无效")
        with self.lock:
            ids, roots = set(), set()
            for entry in entries:
                plugin_id = identifier(entry["id"])
                if not isinstance(entry.get("required", False), bool):
                    raise ValueError("基础模块标记必须为布尔值")
                root = str(Path(entry["root"]).resolve())
                if plugin_id in ids or root in roots:
                    raise ValueError("插件注册表包含重复项")
                ids.add(plugin_id)
                roots.add(root)
            # 删除注册项前必须卸载；避免后台运行的插件变成无法管理的孤儿。
            for plugin_id, manager in self.managers.items():
                if manager.config.get("required") is True:
                    replacement = next((entry for entry in entries if entry["id"] == plugin_id), None)
                    if replacement is None or replacement.get("required") is not True:
                        raise ValueError("基础模块不可移除或降为可选插件")
                if plugin_id not in ids and manager.status()["installed"]:
                    raise ValueError("移除注册项前请先卸载插件")
            for entry in entries:
                plugin_id = entry["id"]
                manager = self.managers.get(plugin_id)
                if manager is None:
                    self.managers[plugin_id] = PluginManager(entry, self.run)
                elif manager.state.get("operation", {}).get("status") != "running":
                    if manager.root != Path(entry["root"]).resolve():
                        raise ValueError("插件根目录不可变更")
                    manager.config.update(entry)
            self.managers = {key: self.managers[key] for key in ids}

    @property
    def running(self):
        return any(m.state.get("operation", {}).get("status") == "running" for m in self.managers.values())

    def status(self):
        self.reload()
        return [m.status() for _, m in sorted(self.managers.items(), key=lambda item: (item[1].config.get("required", False), item[0]))]

    def begin(self, plugin_id, action):
        self.reload()
        with self.lock:
            if self.running:
                raise ValueError("插件操作正在执行")
            manager = self.managers.get(plugin_id)
            if manager is None:
                raise ValueError("未知插件")
            return manager.begin(plugin_id, action)

    def execute(self, plugin_id, action):
        self.managers[plugin_id].execute(action)

    def manifest(self, manager):
        root = manager.root / "runtime"
        return validate(json.loads(resource(root, "plugin.json").read_text()), manager.id, root)

    def request(self, request):
        self.reload()
        if not isinstance(request, dict):
            raise ValueError("插件请求无效")
        principal = request.get("principal", {})
        if not isinstance(principal, dict):
            raise ValueError("插件请求身份无效")
        role = principal.get("role")
        if role not in {"admin", "key", "device"} or not isinstance(principal.get("id"), str):
            raise ValueError("插件请求身份无效")
        kind = request.get("kind")
        if role == "device" and kind != "rpc":
            raise ValueError("设备入口不提供页面或目录")
        if kind == "catalog":
            result = []
            for manager in sorted(self.managers.values(), key=lambda item: (item.config.get("required", False), item.id)):
                if not manager.status()["enabled"]:
                    continue
                try:
                    manifest = self.manifest(manager)
                except (ValueError, OSError):
                    continue  # 无效包不开放任何页面/操作。
                pages = [{k: p[k] for k in ("id", "title", "slot")}
                         for p in manifest.get("pages", []) if role in p["roles"]]
                result.append({"id": manager.id, "name": manager.name,
                               "required": manager.config.get("required", False),
                               "version": manifest["version"], "pages": pages,
                               **presentation(manifest),
                               "capabilities": manifest.get("capabilities", [])})
            return result
        manager = self.managers.get(request.get("pluginId"))
        if manager is None or not manager.status()["enabled"]:
            raise ValueError("插件未启用")
        if not self.capacity.acquire(blocking=False):
            raise ValueError("插件请求繁忙，请稍后重试")
        try:
            # 与生命周期排空共用锁；获得锁后再次检查准入，防止停用竞态。
            with (manager.root / "activity.lock").open("rb") as lock:
                fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
                if not manager.status()["enabled"]:
                    raise ValueError("插件未启用")
                manifest = self.manifest(manager)
                entries = manifest.get("pages" if kind == "page" else "operations", [])
                entry = next((p for p in entries if p["id"] == request.get("name") and role in p["roles"]), None)
                if entry is None or kind not in {"page", "rpc"}:
                    raise ValueError("插件扩展不可访问")
                if kind == "page":
                    return {"html": resource(manager.root / "runtime", entry["entry"]).read_text()}
                return self.rpc(manager, entry["id"], principal, request.get("input"))
        except BlockingIOError as error:
            raise ValueError("插件操作正在执行") from error
        finally:
            self.capacity.release()

    def rpc(self, manager, operation, principal, value):
        payload = json.dumps({"principal": principal, "input": value}).encode()
        if len(payload) > 65536:
            raise ValueError("插件请求过大")
        connection = http.client.HTTPConnection("localhost", timeout=15)
        try:
            connection.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            connection.sock.settimeout(15)
            connection.sock.connect(manager.socket_path)
            connection.request("POST", "/plugins/rpc/" + identifier(operation), payload,
                               {"Content-Type": "application/json"})
            response = connection.getresponse()
            body = response.read(512 * 1024 + 1)
            if response.status != 200 or len(body) > 512 * 1024:
                raise ValueError("插件调用失败")
            return json.loads(body)
        except (OSError, http.client.HTTPException) as error:
            raise ValueError("插件服务暂不可用") from error
        finally:
            connection.close()
