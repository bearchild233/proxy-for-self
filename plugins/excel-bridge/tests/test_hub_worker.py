import asyncio
import copy
import gzip
import json
import time
from types import SimpleNamespace

import httpx
import pytest
from starlette.responses import StreamingResponse

from excel_codex_bridge import excel_upstream, hub_worker, image_generation
from helpers import jwt_with_exp
from helpers import text_stream


def envelope(key="key-a", account="account-a", revision=1):
    return {"binding": {"key_id": key, "account_id": account, "revision": revision, "excel_enabled": True},
            "credential": {"account_id": account, "chatgpt_account_id": f"upstream-{account}",
                           "access_token": jwt_with_exp(2000000000)},
            "body": {"model": excel_upstream.MODEL_ID, "input": "hi", "stream": True}}


def call(app, *, payload=None, path="/internal/responses", method="POST", peer=None, headers=None, content=None):
    async def run():
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=peer), base_url="http://worker") as client:
            return await client.request(method, path, json=payload, headers=headers, content=content)
    return asyncio.run(run())


def harness(status=200, **limits):
    requests, clients = [], []

    def factory(proxy):
        def handle(request):
            requests.append(request)
            return httpx.Response(status, headers={"content-type": "text/event-stream"}, content=text_stream("pong"))
        client = httpx.AsyncClient(transport=httpx.MockTransport(handle))
        clients.append(client)
        return client

    return hub_worker.HubWorker(client_factory=factory, **limits), requests, clients


def test_worker_uses_only_envelope_account_and_cleans_clients(monkeypatch):
    monkeypatch.setenv("CODEX_HOME", "must-not-be-read")
    app, requests, clients = harness()
    response = call(app, payload=envelope())
    assert response.status_code == 200
    assert len(requests) == 1
    assert requests[0].headers["chatgpt-account-id"] == "upstream-account-a"
    assert "pong" in response.text
    assert clients[0].is_closed and app.active == 0
    assert excel_upstream._native_call_namespace.get() == ""


@pytest.mark.parametrize("status", [401, 403])
def test_refused_session_never_falls_back(status):
    app, requests, clients = harness(status)
    assert call(app, payload=envelope()).status_code >= 400
    assert len(requests) == 1 and clients[0].is_closed


@pytest.mark.parametrize("mutate", [
    lambda e: e["binding"].update(excel_enabled=False),
    lambda e: e["binding"].update(revision=0),
    lambda e: e["binding"].update(revision=True),
    lambda e: e["credential"].update(account_id="other"),
    lambda e: e["credential"].update(access_token=jwt_with_exp(time.time() - 5)),
    lambda e: e["body"].update(previous_response_id="resp-other"),
    lambda e: e["body"].update(input=[{"type": "input_file", "file_id": "file-test"}]),
    lambda e: e["credential"].update(proxy="file:///etc/passwd"),
    lambda e: e["credential"].update(access_token="secret value"),
])
def test_invalid_envelope_fails_before_upstream(mutate):
    app, requests, _ = harness()
    body = envelope()
    mutate(body)
    response = call(app, payload=body)
    assert response.status_code == 400 and not requests and app.active == 0
    assert body["credential"]["access_token"] not in response.text


def test_tcp_and_browser_requests_cannot_use_worker():
    app, requests, _ = harness()
    assert call(app, payload=envelope(), peer=("127.0.0.1", 1)).status_code == 403
    assert call(app, payload=envelope(), headers={"origin": "http://localhost"}).status_code == 403
    assert not requests


def test_input_images_follow_original_bridge_adapter():
    from test_images import PNG, codex_body, data_url
    app, requests, clients = harness()
    payload = envelope()
    payload["body"] = {**codex_body(data_url(PNG)), "stream": True}
    response = call(app, payload=payload)
    assert response.status_code == 200
    assert len(requests) == 1 and clients[0].is_closed
    assert data_url(PNG) in requests[0].content.decode()
    assert requests[0].headers["chatgpt-account-id"] == "upstream-account-a"


@pytest.mark.parametrize("operation", ["generations", "edits"])
def test_image_endpoints_reuse_original_adapter_and_frozen_account(operation):
    from test_image_generation import ADDIN_REQUEST, CODEX_REQUEST, DRAWN, PICTURE_URL, form_parts
    requests = []
    def factory(proxy):
        def handle(request):
            requests.append(request)
            return httpx.Response(200, json=DRAWN)
        return httpx.AsyncClient(transport=httpx.MockTransport(handle))
    app = hub_worker.HubWorker(client_factory=factory)
    payload = envelope(account="selected")
    payload["body"] = dict(CODEX_REQUEST)
    if operation == "edits":
        payload["body"]["images"] = [{"image_url": PICTURE_URL}]
    response = call(app, path=f"/internal/images/{operation}", payload=payload)
    assert response.status_code == 200 and response.json() == DRAWN
    assert len(requests) == 1
    request = requests[0]
    assert str(request.url) == getattr(image_generation, f"{operation.upper()}_URL")
    assert request.headers["chatgpt-account-id"] == "upstream-selected"
    if operation == "generations":
        assert json.loads(request.content) == ADDIN_REQUEST
    else:
        assert any(name == "image" for name, _, _ in form_parts(request))


@pytest.mark.parametrize("field,value", [
    ("previous_response_id", "resp-opaque"),
    ("input", [{"role": "user", "content": [{"type": "input_file", "file_id": "file-opaque"}]}]),
])
def test_unsupported_input_has_actionable_non_secret_error(field, value):
    app, requests, _ = harness()
    payload = envelope()
    payload["body"][field] = value
    response = call(app, payload=payload)
    assert response.status_code == 400 and not requests
    assert response.json()["error"]["code"] == "unsupported_excel_input"
    assert "opaque" not in response.text


def test_tool_schema_named_input_file_is_not_misclassified_as_an_attachment():
    app, requests, _ = harness()
    payload = envelope()
    payload["body"]["tools"] = [{"type": "function", "name": "test", "parameters": {"type": "object", "properties": {"type": {"const": "input_file"}}}}]
    assert call(app, payload=payload).status_code == 200
    assert len(requests) == 1


def test_compressed_body_limit_and_capacity():
    app, requests, _ = harness(body_limit=1024)
    body = envelope()
    body["body"]["input"] = "x" * 5000
    response = call(app, content=gzip.compress(json.dumps(body).encode()), headers={"content-encoding": "gzip"})
    assert response.status_code == 413 and not requests and app.active == 0
    app.active = app.max_concurrency
    assert call(app, payload=envelope()).status_code == 503
    assert app.active == app.max_concurrency


def test_worker_catalog_keeps_all_original_model_aliases():
    app, requests, _ = harness()
    response = call(app, path="/internal/catalog", method="GET")
    assert response.status_code == 200
    assert {model["slug"] for model in response.json()["models"]} == set(excel_upstream.MODEL_IDS)
    assert len(excel_upstream.MODEL_IDS) == 12 and not requests


def test_namespace_changes_only_with_own_frozen_identity():
    original = envelope()
    same = copy.deepcopy(original)
    changed = [envelope(key="key-b"), envelope(account="account-b"), envelope(revision=2)]
    rotated = copy.deepcopy(original)
    rotated["credential"]["access_token"] = jwt_with_exp(2000000001)
    changed.append(rotated)
    assert hub_worker._prepare(original)[0] == hub_worker._prepare(same)[0]
    for item in changed:
        assert hub_worker._prepare(original)[0] != hub_worker._prepare(item)[0]


def test_entire_stream_runs_in_bound_namespace(monkeypatch):
    observed = []
    async def response(self, body):
        initial = excel_upstream._native_call_namespace.get()
        async def stream():
            await asyncio.sleep(0)
            observed.append(excel_upstream._native_call_namespace.get())
            yield b"data: done\n\n"
        observed.append(initial)
        return StreamingResponse(stream(), media_type="text/event-stream")
    monkeypatch.setattr(hub_worker.Bridge, "responses", response)
    app, _, _ = harness()
    assert call(app, payload=envelope()).status_code == 200
    assert len(observed) == 2 and observed[0] and observed[0] == observed[1]


def test_cancel_releases_capacity_and_clears_memory(monkeypatch):
    stores = []
    async def response(self, body):
        stores.append(self.reader.store)
        await asyncio.sleep(60)
    monkeypatch.setattr(hub_worker.Bridge, "responses", response)
    app, _, _ = harness(timeout=0.01)
    assert call(app, payload=envelope()).status_code == 504
    assert app.active == 0 and not stores[0].status()["configured"]


def test_replay_cache_obeys_item_and_total_byte_caps(monkeypatch):
    excel_upstream._native_call_cache.clear()
    monkeypatch.setattr(excel_upstream, "_NATIVE_CALL_ITEM_BYTES", 100)
    monkeypatch.setattr(excel_upstream, "_NATIVE_CALL_CACHE_BYTES", 160)
    with excel_upstream.scoped_native_calls("bounded"):
        for index in range(20):
            excel_upstream._remember_native_call({"call_id": str(index), "value": "x" * 40})
        assert sum(map(len, excel_upstream._native_call_cache.values())) <= 160
        excel_upstream._remember_native_call({"call_id": "huge", "value": "x" * 100})
        assert excel_upstream._remembered_native_call("huge") is None
    excel_upstream._native_call_cache.clear()


def test_idle_shutdown_waits_for_active_requests_and_a_fresh_idle_window(monkeypatch):
    clock = [0]
    calls = []
    ticks = []
    monkeypatch.setattr(hub_worker, 'time', SimpleNamespace(monotonic=lambda: clock[0]))
    app = hub_worker.HubWorker(idle_timeout=10)
    app.active = 1
    async def wait(seconds):
        assert not calls
        clock[0] += seconds
        ticks.append(clock[0])
        if len(ticks) == 2:
            app.active = 0
            app.last_activity = clock[0]
    monkeypatch.setattr(hub_worker, 'asyncio', SimpleNamespace(sleep=wait))
    monkeypatch.setattr(hub_worker, 'os', SimpleNamespace(getpid=lambda: 123, kill=lambda pid, sig: calls.append((pid,sig))))
    asyncio.run(app._idle_shutdown())
    assert ticks == [10,20,30]
    assert calls == [(123,hub_worker.signal.SIGTERM)]
