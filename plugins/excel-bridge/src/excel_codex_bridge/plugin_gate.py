"""覆盖整个 ASGI 流生命周期的共享锁；管理器排空时拒绝新请求。"""
import json
from pathlib import Path

from . import sse


class PluginGate:
    def __init__(self, app, state_file):
        self.app = app
        self.state_file = Path(state_file)

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http" or (scope.get("path"), scope.get("method")) == ("/healthz", "GET"):
            # 健康检查不访问上游，由内部 Worker 继续校验私有 socket；
            # 安装器必须在开放准入前验证服务能以非特权用户正常启动。
            return await self.app(scope, receive, send)
        import fcntl
        lock = None
        try:
            lock = (self.state_file.parent / "activity.lock").open("rb")
            fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
            enabled = json.loads(self.state_file.read_text()).get("enabled") is True
        except (OSError, ValueError):
            enabled = False
        try:
            if not enabled:
                return await sse.openai_error_response(503, "Excel plugin is disabled or unavailable", code="excel_plugin_unavailable")(scope, receive, send)
            return await self.app(scope, receive, send)
        finally:
            if lock is not None:
                lock.close()
