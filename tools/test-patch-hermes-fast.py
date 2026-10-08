import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("patcher", Path(__file__).with_name("patch-hermes-fast.py"))
patcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(patcher)

SOURCE = '''def _fast_mode_route_supported(model_id, provider, base_url):
    from urllib.parse import urlparse
    return urlparse(base_url).hostname == 'api.openai.com'
'''


class FastTests(unittest.TestCase):
    def test_only_explicit_https_endpoint_is_admitted(self):
        source = patcher.patched(SOURCE, "https://api.ckcyi.com/v1")
        scope = {"_is_openai_fast_model": lambda model: model == "gpt-6.1-sol"}
        exec(source, scope)
        route = scope["_fast_mode_route_supported"]
        self.assertTrue(route("gpt-6.1-sol", "custom:api-hub-native", "https://api.ckcyi.com/v1/"))
        for url in ["http://api.ckcyi.com/v1", "https://api.ckcyi.com.evil.test/v1", "https://api.ckcyi.com:444/v1", "https://api.ckcyi.com/other", "https://user@api.ckcyi.com/v1", "https://api.ckcyi.com/v1?redirect=evil"]:
            self.assertFalse(route("gpt-6.1-sol", "custom:api-hub-native", url))
        self.assertFalse(route("claude-model", "custom", "https://api.ckcyi.com/v1"))
        self.assertEqual(patcher.patched(source, "https://api.ckcyi.com/v1"), source)

    def test_incompatible_source_rejected(self):
        with self.assertRaises(ValueError):
            patcher.patched("def changed(): pass", "https://api.ckcyi.com/v1")


if __name__ == "__main__":
    unittest.main()
