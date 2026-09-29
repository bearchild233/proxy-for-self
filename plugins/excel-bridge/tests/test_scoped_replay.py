import asyncio
from pathlib import Path
import tempfile
import unittest

from excel_codex_bridge import excel_upstream as upstream


class ScopedReplayTests(unittest.TestCase):
    def setUp(self):
        upstream.keep_native_calls_in(None)
        with upstream._native_call_cache_lock:
            upstream._native_call_cache.clear()

    def tearDown(self):
        upstream.keep_native_calls_in(None)
        with upstream._native_call_cache_lock:
            upstream._native_call_cache.clear()

    def test_same_call_id_cannot_cross_key_scope(self):
        item = {"call_id": "call_same", "arguments": "private-a"}
        with upstream.scoped_native_calls("key-a/account-a/binding-1"):
            upstream._remember_native_call(item)
        with upstream.scoped_native_calls("key-b/account-a/binding-1"):
            self.assertIsNone(upstream._remembered_native_call("call_same"))
        with upstream.scoped_native_calls("key-a/account-a/binding-1"):
            self.assertEqual(upstream._remembered_native_call("call_same"), item)
        self.assertIsNone(upstream._remembered_native_call("call_same"))

    def test_account_or_binding_change_invalidates_replay_namespace(self):
        with upstream.scoped_native_calls("key-a/account-a/binding-1"):
            upstream._remember_native_call({"call_id": "call_same"})
        for scope in ["key-a/account-b/binding-1", "key-a/account-a/binding-2"]:
            with upstream.scoped_native_calls(scope):
                self.assertIsNone(upstream._remembered_native_call("call_same"))

    def test_async_requests_keep_independent_namespaces(self):
        async def worker(scope):
            with upstream.scoped_native_calls(scope):
                upstream._remember_native_call({"call_id": "call_async", "value": scope})
                await asyncio.sleep(0)
                return upstream._remembered_native_call("call_async")["value"]

        async def run():
            return await asyncio.gather(worker("a"), worker("b"))

        self.assertEqual(asyncio.run(run()), ["a", "b"])

    def test_scoped_sqlite_replay_isolated_from_legacy_and_other_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            upstream.keep_native_calls_in(Path(directory) / "replay.sqlite")
            with upstream.scoped_native_calls("a"):
                upstream._remember_native_call({"call_id": "call_db", "value": "a"})
            with upstream._native_call_cache_lock:
                upstream._native_call_cache.clear()
            with upstream.scoped_native_calls("b"):
                self.assertIsNone(upstream._remembered_native_call("call_db"))
            self.assertIsNone(upstream._remembered_native_call("call_db"))
            with upstream.scoped_native_calls("a"):
                self.assertEqual(upstream._remembered_native_call("call_db")["value"], "a")

    def test_scope_restores_after_exception_and_rejects_missing_scope(self):
        with self.assertRaises(ValueError):
            with upstream.scoped_native_calls(""):
                pass
        with self.assertRaises(RuntimeError):
            with upstream.scoped_native_calls("a"):
                raise RuntimeError("test")
        self.assertEqual(upstream._native_call_namespace.get(), "")
