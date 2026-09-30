import asyncio
import json
import os
from pathlib import Path
import tempfile
import unittest

from excel_codex_bridge.plugin_gate import PluginGate


@unittest.skipUnless(os.name == 'posix', 'Managed plugins require POSIX')
class PluginGateTests(unittest.IsolatedAsyncioTestCase):
    async def test_gate_holds_lock_until_stream_finishes_and_rejects_disabled(self):
        import fcntl
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / 'state.json'
            state.write_text(json.dumps({'enabled': True}))
            (root / 'activity.lock').touch()
            started, finish = asyncio.Event(), asyncio.Event()
            calls, messages = [], []

            async def app(scope, receive, send):
                calls.append(1)
                started.set()
                await finish.wait()

            async def send(message):
                messages.append(message)

            gate = PluginGate(app, state)
            task = asyncio.create_task(gate({'type': 'http'}, None, send))
            await started.wait()
            with (root / 'activity.lock').open('rb') as lock:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                state.write_text(json.dumps({'enabled': False}))
                await gate({'type': 'http'}, None, send)
                self.assertEqual(messages[0]['status'], 503)
                self.assertEqual(len(calls), 1)
                finish.set()
                await task
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                state.write_text(json.dumps({'enabled': True}))
                messages.clear()
                await gate({'type': 'http'}, None, send)
                self.assertEqual(messages[0]['status'], 503)

    async def test_missing_state_fails_closed(self):
        async def app(*args):
            self.fail('Unavailable plugin reached the upstream app')

        messages = []

        async def send(message):
            messages.append(message)

        with tempfile.TemporaryDirectory() as directory:
            await PluginGate(app, Path(directory) / 'missing')({'type': 'http'}, None, send)
        self.assertEqual(messages[0]['status'], 503)
