import base64
import json
import time
import httpx

def jwt_with_exp(exp: float, **claims) -> str:
    def encode(payload: dict) -> str:
        raw = json.dumps(payload, separators=(",", ":")).encode()
        return base64.urlsafe_b64encode(raw).decode().rstrip("=")

    return f"{encode({'alg': 'none'})}.{encode({'exp': int(exp), **claims})}.sig"

def session_headers(exp: float | None = None, account: str = "account-id") -> dict[str, str]:
    token = jwt_with_exp(exp if exp is not None else time.time() + 3600)
    return {"authorization": f"Bearer {token}", "chatgpt-account-id": account}

def text_stream(text: str) -> bytes:
    return b"".join(
        [
            sse("response.created", {"type": "response.created", "response": {"id": "resp_1"}}),
            sse(
                "response.output_text.delta",
                {"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0,
                 "content_index": 0, "delta": text},
            ),
            sse(
                "response.completed",
                {"type": "response.completed", "response": {
                    "id": "resp_1", "status": "completed", "model": "gpt-5.6-sol",
                    "output": [{"type": "message", "role": "assistant", "id": "msg_1",
                                "content": [{"type": "output_text", "text": text}]}],
                }},
            ),
        ]
    )


import asyncio
from starlette.applications import Starlette
from starlette.routing import Route
from starlette.responses import Response
from excel_codex_bridge.server import Bridge
from excel_codex_bridge.hub_worker import BoundReader
from excel_codex_bridge.excel_upstream import ExcelSessionStore

class StaticReader(BoundReader):
    def __init__(self):
        super().__init__(ExcelSessionStore(None))

def create_app(reader, *, client_factory):
    """Test-only HTTP adapter; production serves only the private worker envelope."""
    bridge = Bridge(reader, client_factory)
    async def responses(request):
        return await bridge.responses(await request.json())
    async def images(request):
        if request.path_params["operation"] not in {"generations", "edits"}:
            return Response(status_code=404)
        return await bridge.images(request.path_params["operation"], await request.json())
    return Starlette(routes=[Route("/v1/responses", responses, methods=["POST"]), Route("/responses", responses, methods=["POST"]), Route("/v1/images/{operation}", images, methods=["POST"]), Route("/images/{operation}", images, methods=["POST"])])

def sse(event: str, payload: dict) -> bytes:
    return f"event: {event}\ndata: {json.dumps(payload)}\n\n".encode()

def ok_stream(_request):
    return httpx.Response(200, headers={"content-type": "text/event-stream"}, content=text_stream("pong"))

class BridgeHarness:
    def __init__(self, handler, *, configured: bool = True, exp: float | None = None,
                 timeout: httpx.Timeout = httpx.Timeout(5.0)):
        self.upstream_requests: list[httpx.Request] = []

        def record(request: httpx.Request) -> httpx.Response:
            self.upstream_requests.append(request)
            return handler(request)

        self.reader = StaticReader()
        if configured:
            self.reader.store.configure(
                session_headers(exp), persist=False, allow_expired=True
            )
        self.app = create_app(
            self.reader,
            client_factory=lambda: httpx.AsyncClient(transport=httpx.MockTransport(record), timeout=timeout),
        )

    def request(self, method: str, path: str, *, client_host="127.0.0.1", **kwargs) -> httpx.Response:
        async def run():
            transport = httpx.ASGITransport(app=self.app, client=(client_host, 50000))
            async with httpx.AsyncClient(transport=transport, base_url="http://127.0.0.1:8765") as client:
                return await client.request(method, path, **kwargs)

        return asyncio.run(run())

    def upstream_json(self, index: int = -1) -> dict:
        return json.loads(self.upstream_requests[index].content)
