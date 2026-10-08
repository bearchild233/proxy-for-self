"""纯 UI 插件的版本存储。切换只原子替换状态文件，不管理网关或 Worker 进程。"""
import hashlib
from contextlib import contextmanager
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import tarfile
import tempfile
import threading
import uuid
import time
from datetime import datetime

ID = re.compile(r"[a-z][a-z0-9-]{0,63}\Z")
VERSION = re.compile(r"[0-9a-f]{64}\Z")
MAX_FILE = 4 * 1024 * 1024
MAX_BUNDLE = 16 * 1024 * 1024

class PluginVersionChangedError(ValueError):
    pass


def relative(name):
    if not isinstance(name, str):
        raise ValueError("资源路径无效")
    path = PurePosixPath(name)
    if not name or path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name or str(path) != name:
        raise ValueError("资源路径无效")
    return name


class StaticPluginStore:
    def __init__(self, root, policies):
        self.root = Path(root).resolve()
        self.root.mkdir(parents=True, exist_ok=True)
        self.policies = policies
        self.lock = threading.RLock()
        for plugin_id, policy in policies.items():
            if not ID.fullmatch(plugin_id) or not isinstance(policy.get("required", False), bool):
                raise ValueError("宿主插件策略无效")

    @contextmanager
    def mutation(self):
        # CLI 安装与 HTTP 管理可能处于不同进程，必须共用同一写锁。
        with self.lock, (self.root / '.mutation.lock').open('a+b') as handle:
            if os.name == 'nt':
                import msvcrt
                if handle.seek(0, os.SEEK_END) == 0:
                    handle.write(b'0')
                    handle.flush()
                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_LOCK, 1)
            else:
                import fcntl
                fcntl.flock(handle, fcntl.LOCK_EX)
            try:
                yield
            finally:
                if os.name == 'nt':
                    handle.seek(0)
                    msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
                else:
                    fcntl.flock(handle, fcntl.LOCK_UN)

    def directory(self, plugin_id):
        if plugin_id not in self.policies:
            raise ValueError("未注册插件")
        path = self.root / plugin_id
        if path.is_symlink():
            raise ValueError("插件目录无效")
        path.mkdir(exist_ok=True)
        return path

    def state(self, plugin_id):
        path = self.directory(plugin_id) / "active.json"
        if path.is_symlink():
            raise ValueError("插件状态路径无效")
        return json.loads(path.read_text()) if path.exists() else {"enabled": False, "current": None, "previous": None}

    def save(self, plugin_id, state):
        path = self.directory(plugin_id) / "active.json"
        temporary = path.with_name(".active-" + uuid.uuid4().hex)
        try:
            with temporary.open("x", encoding="utf-8") as output:
                json.dump(state, output)
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)

    def validate(self, plugin_id, root):
        if root.is_symlink():
            raise ValueError("版本目录无效")
        path = root / "plugin.json"
        if not path.is_file() or path.stat().st_size > 65536:
            raise ValueError("清单缺失或过大")
        manifest = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(manifest, dict) or manifest.get("id") != plugin_id or manifest.get("protocol") != 2 or manifest.get("kind") != "ui":
            raise ValueError("插件协议不兼容")
        if manifest.get('sdk', 1) != 1:
            raise ValueError('插件 SDK 不兼容')
        runtime = manifest.get('runtime', 'iframe')
        if runtime not in {'iframe', 'vue-component'}:
            raise ValueError('插件运行方式不兼容')
        if runtime == 'vue-component' and (not self.policies[plugin_id].get('trustedUi') or manifest.get('hostApi') != 1):
            raise ValueError('组件插件未授权或宿主接口不兼容')
        if not isinstance(manifest.get("version"), str) or not manifest["version"]:
            raise ValueError("插件版本无效")
        notes = manifest.get('releaseNotes', [])
        if not isinstance(notes, list) or len(notes) > 30 or any(not isinstance(note, str) or not 0 < len(note) <= 1000 for note in notes):
            raise ValueError('更新说明无效')
        if 'releasedAt' in manifest:
            try:
                published = datetime.fromisoformat(manifest['releasedAt'].replace('Z', '+00:00'))
                if published.tzinfo is None:
                    raise ValueError('发布时间缺少时区')
            except (ValueError, TypeError, AttributeError) as error:
                raise ValueError('发布时间无效') from error
        resources = manifest.get("resources", {})
        if not isinstance(resources, dict) or not 0 < len(resources) <= 64:
            raise ValueError("资源清单无效")
        for name, digest in resources.items():
            item = root / relative(name)
            if any(parent.is_symlink() for parent in item.parents if parent != root and root in parent.parents):
                raise ValueError("资源目录无效")
            if item.is_symlink() or not item.is_file() or item.stat().st_size > MAX_FILE:
                raise ValueError("资源缺失或过大")
            if not isinstance(digest, str) or hashlib.sha256(item.read_bytes()).hexdigest() != digest:
                raise ValueError("资源校验失败")
        worker = manifest.get("worker")
        if worker is not None and (not isinstance(worker, str) or worker not in resources or not worker.endswith('.py') or not self.policies[plugin_id].get('worker')):
            raise ValueError("后台逻辑未授权")
        policy_worker = manifest.get('policyWorker')
        if policy_worker is not None and (plugin_id not in {'backup', 'pricing'} or policy_worker != 'worker/policy.py' or policy_worker not in resources):
            raise ValueError('策略后台未授权')
        pages = manifest.get("pages", [])
        if not isinstance(pages, list) or not 0 < len(pages) <= 16:
            raise ValueError("页面声明无效")
        page_ids = set()
        for page in pages:
            if not isinstance(page, dict) or not isinstance(page.get("id"), str) or not ID.fullmatch(page["id"]) or page["id"] in page_ids:
                raise ValueError("页面标识无效")
            page_ids.add(page["id"])
            if not isinstance(page.get("entry"), str) or page["entry"] not in resources or not page["entry"].endswith('.js' if runtime == 'vue-component' else '.html'):
                raise ValueError("页面资源未声明")
            if not isinstance(page.get("title"), str) or not isinstance(page.get("roles"), list) or not page["roles"] or not all(isinstance(role, str) and role in {"admin", "key"} for role in page["roles"]):
                raise ValueError("页面身份无效")
        for page in pages:
            route=page.get('route')
            if route is not None and (not isinstance(route,str) or not route.startswith('/') or route.startswith('//') or '?' in route or '#' in route):
                raise ValueError('页面路由无效')
        capabilities = manifest.get("capabilities", [])
        if not isinstance(capabilities, list) or not all(isinstance(cap, str) for cap in capabilities) or not set(capabilities) <= set(self.policies[plugin_id].get("capabilities", [])):
            raise ValueError("插件能力超出宿主授权")
        return manifest

    def install(self, plugin_id, archive, expected_sha256):
        with self.mutation():
            directory = self.directory(plugin_id)
            archive = Path(archive)
            if archive.stat().st_size > MAX_BUNDLE:
                raise ValueError("插件包过大")
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            if digest != expected_sha256:
                raise ValueError("插件包校验失败")
            releases = directory / "releases"
            if releases.is_symlink():
                raise ValueError("版本目录无效")
            releases.mkdir(exist_ok=True)
            with tempfile.TemporaryDirectory(prefix=".install-", dir=directory) as temporary:
                stage = Path(temporary)
                with tarfile.open(archive, "r:gz") as package:
                    total, seen = 0, set()
                    for item in package:
                        name = relative(item.name)
                        total += item.size
                        if not item.isfile() or name in seen or len(seen) >= 65 or item.size > MAX_FILE or total > MAX_BUNDLE:
                            raise ValueError("插件包内容无效")
                        seen.add(name)
                        target = stage / name
                        target.parent.mkdir(parents=True, exist_ok=True)
                        with package.extractfile(item) as source, target.open("xb") as output:
                            shutil.copyfileobj(source, output)
                manifest = self.validate(plugin_id, stage)
                if seen != set(manifest["resources"]) | {"plugin.json"}:
                    raise ValueError("包包含未声明资源")
                # 网关以独立用户读取策略包；临时目录默认 0700，发布前显式开放组只读。
                for item in stage.rglob('*'):
                    item.chmod(0o750 if item.is_dir() else 0o640)
                stage.chmod(0o750)
                destination = releases / digest
                if not destination.exists():
                    # 同一文件系统原子移动完整目录，不留下半个版本。
                    os.replace(stage, destination)
                else:
                    self.validate(plugin_id, destination)
                state = self.state(plugin_id)
                if state["current"] == digest:
                    self.cleanup(plugin_id, state)
                    return state
                state = {"current": digest, "previous": state["current"],
                         "enabled": bool(self.policies[plugin_id].get("required") or not state["current"] or state["enabled"])}
                self.save(plugin_id, state)
                self.cleanup(plugin_id, state)
                return state

    def cleanup(self, plugin_id, state):
        # 仅清理由当前存储管理的版本目录；保留活动版和一个回退版本。
        releases = self.directory(plugin_id) / "releases"
        if not releases.exists():
            return
        if releases.is_symlink():
            raise ValueError("版本目录无效")
        keep = {state["current"], state["previous"]} | self.leased_versions(plugin_id)
        for item in releases.iterdir():
            if VERSION.fullmatch(item.name) and item.name not in keep:
                if item.is_symlink() or item.resolve().parent != releases.resolve():
                    raise ValueError("版本路径无效")
                shutil.rmtree(item)

    def leased_versions(self, plugin_id):
        directory = self.directory(plugin_id) / '.leases'
        directory.mkdir(exist_ok=True)
        versions = set()
        for path in directory.glob('*.json'):
            try: data = json.loads(path.read_text())
            except FileNotFoundError: continue
            if data['expires'] > time.time():
                versions.add(data['version'])
            else:
                path.unlink(missing_ok=True)
        return versions

    def lease(self, plugin_id, version, lease_id=None, release=False):
        with self.mutation():
            self.leased_versions(plugin_id)
            directory = self.directory(plugin_id) / '.leases'
            if lease_id is None:
                self.manifest(plugin_id, version)
                if len(list(directory.glob('*.json'))) >= 128:
                    raise ValueError('打开的插件页面过多')
                lease_id = uuid.uuid4().hex
            if not re.fullmatch(r'[0-9a-f]{32}', lease_id):
                raise ValueError('版本租约无效')
            path = directory / (lease_id + '.json')
            if release:
                path.unlink(missing_ok=True)
                self.cleanup(plugin_id, self.state(plugin_id))
                return None
            if not self.state(plugin_id)['enabled']:
                raise ValueError('插件未启用')
            self.manifest(plugin_id, version)
            temporary = directory / (lease_id + '.tmp')
            temporary.write_text(json.dumps({'version':version,'expires':time.time()+120}))
            temporary.replace(path)
            return lease_id

    def manifest(self, plugin_id, version=None):
        state = self.state(plugin_id)
        version = version or state["current"]
        if not isinstance(version, str) or not VERSION.fullmatch(version) or version not in ({state["current"], state["previous"]} | self.leased_versions(plugin_id)):
            raise ValueError("版本不可访问，请重新打开页面")
        return self.validate(plugin_id, self.directory(plugin_id) / "releases" / version)

    def page(self, plugin_id, version, page_id, role):
        with self.lock:
            if not self.state(plugin_id)["enabled"]:
                raise ValueError("插件未启用")
            manifest = self.manifest(plugin_id, version)
            page = next((item for item in manifest["pages"] if item["id"] == page_id and role in item["roles"]), None)
            if page is None:
                raise ValueError("页面不可访问")
            return (self.directory(plugin_id) / "releases" / version / page["entry"]).read_text(encoding="utf-8")

    def catalog(self, role):
        with self.lock:
            result = []
            for plugin_id, policy in self.policies.items():
                state = self.state(plugin_id)
                if not state["enabled"]:
                    continue
                manifest = self.manifest(plugin_id)
                pages = [{key: page[key] for key in ("id", "title")} for page in manifest["pages"] if role in page["roles"]]
                if pages:
                    result.append({"id": plugin_id, "name": policy["name"], "required": policy.get("required", False),
                                   "version": manifest["version"], "contentVersion": state["current"], "pages": pages})
            return sorted(result, key=lambda item: (item["required"], item["id"]))

    def action(self, plugin_id, action, expected_current=None, expected_previous=None):
        with self.mutation():
            if action not in {"enable", "disable", "uninstall", "rollback"}:
                raise ValueError("未知操作")
            self.directory(plugin_id)
            policy = self.policies[plugin_id]
            if policy.get("required") and action in {"disable", "uninstall"}:
                raise ValueError("基础模块不可停用或卸载")
            state = self.state(plugin_id)
            if action == "rollback":
                if ((expected_current is not None and state['current'] != expected_current)
                        or (expected_previous is not None and state['previous'] != expected_previous)):
                    raise PluginVersionChangedError('插件版本已变化，请刷新后重新确认回退目标')
                if not state["previous"]:
                    raise ValueError("没有可回退版本")
                self.manifest(plugin_id, state["previous"])
                state["current"], state["previous"] = state["previous"], state["current"]
            elif action == "enable":
                self.manifest(plugin_id)
                state["enabled"] = True
            else:
                state["enabled"] = False
                if action == "uninstall":
                    state.update(current=None, previous=None)
            self.save(plugin_id, state)
            self.cleanup(plugin_id, state)
            return state
