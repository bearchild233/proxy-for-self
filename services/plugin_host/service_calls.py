"""服务插件管理调用转发：页面的 <插件ID>.* 能力经管理连接原样转给插件进程的 admin.call。

socket 只从 root 管理的内核注册表读取，不接受页面或清单提供的路径。
"""
import json
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / 'services/updater'))
import kernel_protocol  # noqa: E402

# 任务给出的管理调用超时。
CALL_TIMEOUT_SECONDS = 10


class ServiceConflictError(Exception):
    """服务配置版本冲突；保留草稿，由管理员显式重新读取。"""


class ServiceCalls:
    def __init__(self, registry_path):
        self.registry = Path(registry_path)

    def socket(self, plugin_id):
        try:
            value = json.loads(self.registry.read_text(encoding='utf-8'))
        except (OSError, ValueError) as error:
            raise ConnectionError('内核注册表不可读') from error
        entries = value.get('plugins') if isinstance(value, dict) else None
        entry = next((item for item in entries or [] if isinstance(item, dict) and item.get('id') == plugin_id), None)
        if entry is None or entry.get('enabled') is not True:
            raise ConnectionError('插件服务未启用')
        path = entry.get('socket')
        if not isinstance(path, str) or not Path(path).is_absolute():
            raise ConnectionError('插件服务地址无效')
        return path

    def call(self, plugin_id, method, params, role):
        path = self.socket(plugin_id)
        try:
            return kernel_protocol.admin_call(path, method, params, role, CALL_TIMEOUT_SECONDS, expected_id=plugin_id)
        except kernel_protocol.RpcFailure as error:
            # 插件的错误码映射到控制面的 HTTP 语义：参数错误 400，未实现或禁止 403，其余按暂不可用 503。
            if error.code == 'invalid_params':
                raise ValueError(error.message) from error
            if error.code == 'conflict':
                raise ServiceConflictError('配置已被更新，请重新读取后保存') from error
            if error.code in {'not_supported', 'forbidden'}:
                raise PermissionError('插件没有此能力') from error
            raise ConnectionError('插件服务暂不可用') from error
        except OSError as error:
            raise ConnectionError('插件服务暂不可用') from error
