"""插件平台 v1 合同。安装配置由维护者提供，包只能声明受限的 UI/RPC 能力。"""
from pathlib import Path, PurePosixPath
import re

ID = re.compile(r"[a-z][a-z0-9-]{0,63}\Z")
SLOTS = {"settings", "key-usage", "key-editor", "key-config", "navigation",
         "account-menu", "account-detail", "group-menu", "key-menu",
         "proxy-menu", "usage-detail", "dashboard"}
ADMIN_SLOTS = SLOTS - {"settings", "key-usage", "key-editor", "key-config"}
CATEGORIES = {"management", "account-tools", "client-access", "analytics", "extensions", "operations"}


def presentation(manifest):
    """只返回展示字段；不把 worker 路径、操作权限或运行配置交给页面。"""
    return {"category": manifest.get("category", "extensions"),
            "details": manifest.get("details", {})}


def validate_presentation(manifest):
    category = manifest.get("category", "extensions")
    if not isinstance(category, str) or category not in CATEGORIES:
        raise ValueError("插件分类无效")
    details = manifest.get("details", {})
    if not isinstance(details, dict) or set(details) - {"overview", "features", "data", "disableEffect", "updateEffect"}:
        raise ValueError("插件详情无效")
    for name, value in details.items():
        if name == "features":
            if not isinstance(value, list) or len(value) > 12 or not all(isinstance(item, str) and 0 < len(item) <= 160 for item in value):
                raise ValueError("插件功能说明无效")
        elif not isinstance(value, str) or len(value) > 2000:
            raise ValueError("插件详情无效")


def identifier(value):
    if not isinstance(value, str) or not ID.fullmatch(value):
        raise ValueError("插件标识无效")
    return value


def resource(root, name):
    path = PurePosixPath(name)
    if (not name or path.is_absolute() or ".." in path.parts or "\\" in name
            or str(path) != name):
        raise ValueError("插件资源路径无效")
    target = root / name
    if target.is_symlink() or not target.resolve().is_relative_to(root.resolve()):
        raise ValueError("插件资源路径越界")
    if not target.is_file() or target.stat().st_size > 512 * 1024:
        raise ValueError("插件资源不存在或过大")
    return target


def validate(manifest, plugin_id, root: Path, version=None):
    if not isinstance(manifest, dict):
        raise ValueError("插件清单无效")
    if (manifest.get("id") != plugin_id or manifest.get("protocol") != 1
            or not isinstance(manifest.get("version"), str)
            or (version is not None and manifest["version"] != version)):
        raise ValueError("插件协议或版本不兼容")
    validate_presentation(manifest)
    capabilities = manifest.get("capabilities", [])
    if not isinstance(capabilities, list) or len(capabilities) > 32:
        raise ValueError("插件能力清单无效")
    for capability in capabilities:
        identifier(capability)
    for kind in ("pages", "operations"):
        entries = manifest.get(kind, [])
        if not isinstance(entries, list) or len(entries) > 32:
            raise ValueError("插件扩展清单无效")
        seen = set()
        for entry in entries:
            if not isinstance(entry, dict):
                raise ValueError("插件扩展清单无效")
            name = identifier(entry.get("id"))
            if name in seen:
                raise ValueError("插件扩展标识重复")
            seen.add(name)
            roles = entry.get("roles", [])
            if not roles or not isinstance(roles, list) or not all(isinstance(role, str) for role in roles) or not set(roles) <= ({"admin", "key"} if kind == "pages" else {"admin", "key", "device"}):
                raise ValueError("插件权限声明无效")
            if kind == "pages":
                if entry.get("slot") not in SLOTS or not isinstance(entry.get("title"), str):
                    raise ValueError("插件页面声明无效")
                if entry["slot"] in ADMIN_SLOTS and set(roles) != {"admin"}:
                    raise ValueError("管理页面仅限管理员")
                resource(root, entry.get("entry", ""))
    return manifest
