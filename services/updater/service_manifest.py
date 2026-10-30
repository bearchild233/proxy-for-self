"""服务插件清单段 manifest.service 的校验。控制面与 updater 各自调用，不互相信任。

只读取包内文件头与清单字段，不执行二进制。
"""
from __future__ import annotations

from pathlib import Path
import re

# 内核协议已定义的能力名，与 backend/crates/gateway-host/src/plugin/registry.rs 的 grants 白名单一致。
KERNEL_CAPABILITIES = frozenset({"log", "upstream.probe", "http.fetch", "accounts.read", "groups.move", "quota.read", "accounts.warmup.read", "accounts.warmup.run"})
# 与 kernel-protocol 的 is_compatible 对齐：内核主版本 0，插件次版本下限 4，上限不超过内核次版本。
KERNEL_API_MAJOR = 0
KERNEL_API_MIN_MINOR = 4
KERNEL_API_MINOR = 8
SERVICE_FIELDS = frozenset({"binary", "kernelApi", "capabilities", "fetchHosts", "memoryMax", "shutdownSeconds"})
MEMORY_MIN_MIB = 16
MEMORY_MAX_MIB = 512
SHUTDOWN_MIN_SECONDS = 5
SHUTDOWN_MAX_SECONDS = 600
BINARY = re.compile(r"service/[a-z][a-z0-9_-]*")
MEMORY = re.compile(r"([0-9]{1,4})M")
HOST = re.compile(r"(?=.{1,253}\Z)[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)*")
ELF_MAGIC = b"\x7fELF"
ELFCLASS64 = 2
ELFDATA2LSB = 1
ET_EXEC = 2
ET_DYN = 3  # PIE 可执行文件（Rust 默认产物）
EM_X86_64 = 62


def is_x86_64_elf(path: Path) -> bool:
    """检查 ELF 魔数、64 位小端、可执行类型与 x86-64 机器类型。"""
    try:
        with path.open("rb") as stream:
            header = stream.read(20)
    except OSError:
        return False
    if len(header) < 20 or header[:4] != ELF_MAGIC or header[4] != ELFCLASS64 or header[5] != ELFDATA2LSB:
        return False
    e_type = int.from_bytes(header[16:18], "little")
    e_machine = int.from_bytes(header[18:20], "little")
    return e_type in (ET_EXEC, ET_DYN) and e_machine == EM_X86_64


def _integer(value, low: int, high: int) -> bool:
    # bool 是 int 的子类，必须排除。
    return type(value) is int and low <= value <= high


def validate_service(manifest: dict, root: Path) -> dict | None:
    """校验清单中的 service 段。无该段返回 None；有则返回规范化的字段。

    root 为插件包根目录（已解包或已安装的版本目录），二进制必须位于其中且列入 resources。
    """
    if "service" not in manifest:
        return None
    service = manifest["service"]
    if not isinstance(service, dict) or set(service) != SERVICE_FIELDS:
        raise ValueError("服务段字段无效")
    binary = service["binary"]
    if not isinstance(binary, str) or not BINARY.fullmatch(binary):
        raise ValueError("服务二进制路径无效")
    resources = manifest.get("resources")
    if not isinstance(resources, dict) or binary not in resources:
        raise ValueError("服务二进制未列入资源清单")
    file = root / binary
    if file.is_symlink() or not file.is_file() or not is_x86_64_elf(file):
        raise ValueError("服务二进制不是 x86-64 ELF 可执行文件")

    api = service["kernelApi"]
    if (not isinstance(api, dict) or set(api) != {"major", "minMinor"}
            or not _integer(api["major"], KERNEL_API_MAJOR, KERNEL_API_MAJOR)
            or not _integer(api["minMinor"], KERNEL_API_MIN_MINOR, KERNEL_API_MINOR)):
        raise ValueError("服务所需内核接口版本不兼容")

    capabilities = service["capabilities"]
    if (not isinstance(capabilities, list) or len(set(capabilities)) != len(capabilities)
            or not all(isinstance(item, str) and item in KERNEL_CAPABILITIES for item in capabilities)):
        raise ValueError("服务申请的能力无效")
    if manifest.get("id") != "account-warmup" and any(item.startswith("accounts.warmup.") for item in capabilities):
        raise ValueError("账号预热能力只允许 account-warmup")

    hosts = service["fetchHosts"]
    if (not isinstance(hosts, list) or len(set(hosts)) != len(hosts)
            or not all(isinstance(item, str) and HOST.fullmatch(item) for item in hosts)):
        raise ValueError("服务 fetchHosts 无效")

    memory = service["memoryMax"]
    match = MEMORY.fullmatch(memory) if isinstance(memory, str) else None
    if not match or not MEMORY_MIN_MIB <= int(match.group(1)) <= MEMORY_MAX_MIB:
        raise ValueError("服务内存上限无效")

    shutdown = service["shutdownSeconds"]
    if not _integer(shutdown, SHUTDOWN_MIN_SECONDS, SHUTDOWN_MAX_SECONDS):
        raise ValueError("服务关停超时无效")

    return {"binary": binary, "name": Path(binary).name, "kernelApi": {"major": api["major"], "minMinor": api["minMinor"]},
            "capabilities": list(capabilities), "fetchHosts": list(hosts),
            "memoryMaxMiB": int(match.group(1)), "shutdownSeconds": shutdown}
