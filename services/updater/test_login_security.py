import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch
from login_security import LoginSecurity, validate_policy, DEFAULTS


class LoginSecurityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.config = {"database": str(root / "bans.db"), "policy_file": str(root / "policy.local")}
        self.security = LoginSecurity(self.config)

    def test_bans_persist_are_site_scoped_and_expire(self):
        with patch("login_security.time.time", return_value=1000):
            self.security.change_ban("api", "198.51.100.1", 900)
            other = LoginSecurity(self.config)
            self.assertEqual(other.check("api", "198.51.100.1"), 900)
            self.assertEqual(other.check("panel", "198.51.100.1"), 0)
            self.assertEqual(other.check("api", "198.51.100.2"), 0)
        with patch("login_security.time.time", return_value=1901):
            self.assertEqual(other.check("api", "198.51.100.1"), 0)

    def test_unban_normalizes_ipv6_and_cannot_ban_proxy(self):
        self.security.change_ban("panel", "::ffff:198.51.100.1", 900)
        self.assertGreater(self.security.check("panel", "198.51.100.1"), 0)
        self.security.change_ban("panel", "198.51.100.1", 0)
        self.assertEqual(self.security.check("panel", "198.51.100.1"), 0)
        for ip in ["127.0.0.1", "::1", "::ffff:127.0.0.1", "0.0.0.0", "1.2.3.4;cmd", "1.2.3.4,5.6.7.8"]:
            with self.assertRaises(ValueError):
                self.security.change_ban("api", ip, 900)

    def test_blocked_source_does_not_drain_global_window(self):
        for _ in range(10):
            self.assertEqual(self.security.check("api", "198.51.100.1"), 0)
        for _ in range(250):
            self.assertGreater(self.security.check("api", "198.51.100.1"), 0)
        self.assertEqual(self.security.check("api", "198.51.100.2"), 0)

    def test_policy_validation_and_failed_reload_restore(self):
        for extra in [{"maxFailures": True}, {"maxFailures": 0}, {"maxBanSeconds": 60}, {"site": "sshd"}, {"banSeconds": 1.5}]:
            with self.assertRaises(ValueError):
                validate_policy({"site": "api", **DEFAULTS, **extra})
        path = Path(self.config["policy_file"])
        path.write_text("[proxy-api-login]\nmaxretry = 5\n")
        with patch.object(self.security, "command", side_effect=[ValueError("invalid"), "ok"]):
            with self.assertRaises(ValueError):
                self.security.set_policy({"site": "api", **DEFAULTS, "maxFailures": 6})
        self.assertEqual(path.read_text(), "[proxy-api-login]\nmaxretry = 5\n")


if __name__ == "__main__":
    unittest.main()
