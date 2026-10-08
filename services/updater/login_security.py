"""Login-only enforcement for fail2ban; no firewall changes or proxy reloads on bans.

The updater's privileged socket manages fixed jails. A separate read-only socket
serves Caddy's forward_auth checks; it cannot invoke any administrative operation.
"""
from __future__ import annotations

import argparse
import configparser
import grp
import ipaddress
import json
import os
from pathlib import Path
import socketserver
import sqlite3
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler

JAILS = {"api": "proxy-api-login", "panel": "proxy-panel-login"}
DEFAULTS = {"maxFailures": 5, "windowSeconds": 600, "banSeconds": 900, "maxBanSeconds": 86400}


def valid_ip(value):
    ip = ipaddress.ip_address(value)
    if isinstance(ip, ipaddress.IPv6Address) and ip.ipv4_mapped:
        ip = ip.ipv4_mapped
    return str(ip)


def validate_policy(value):
    if not isinstance(value, dict) or set(value) != {"site", *DEFAULTS} or value["site"] not in JAILS:
        raise ValueError("Invalid policy")
    for key, low, high in [("maxFailures", 3, 20), ("windowSeconds", 60, 3600),
                           ("banSeconds", 60, 86400), ("maxBanSeconds", 60, 604800)]:
        if type(value[key]) is not int or not low <= value[key] <= high:
            raise ValueError("Invalid threshold")
    if value["maxBanSeconds"] < value["banSeconds"]:
        raise ValueError("Invalid maximum ban")
    return value


def atomic_text(path, text, mode=0o600):
    temp = path.with_suffix(path.suffix + ".tmp")
    with temp.open("w") as output:
        os.chmod(temp, mode)
        output.write(text)
        output.flush()
        os.fsync(output.fileno())
    os.replace(temp, path)


class LoginSecurity:
    def __init__(self, config):
        self.config = config
        self.database = Path(config["database"])
        self.database.parent.mkdir(parents=True, exist_ok=True)
        self.lock = threading.RLock()
        self.burst = {}
        self.burst_lock = threading.Lock()
        with self.connect() as db:
            db.execute("CREATE TABLE IF NOT EXISTS bans (site TEXT, ip TEXT, expires INTEGER NOT NULL, PRIMARY KEY(site,ip))")
        os.chmod(self.database, 0o600)

    def connect(self):
        return sqlite3.connect(self.database, timeout=3)

    def command(self, *args):
        result = subprocess.run(["/usr/bin/fail2ban-client", *args], capture_output=True, text=True, timeout=8)
        if result.returncode:
            raise ValueError("Login protection service unavailable")
        return result.stdout.strip()

    def policies(self):
        parser = configparser.ConfigParser(interpolation=None)
        parser.read(self.config["policy_file"])
        result = []
        for site, jail in JAILS.items():
            result.append({"site": site, **{key: parser.getint(jail, field, fallback=DEFAULTS[key])
                for key, field in [("maxFailures", "maxretry"), ("windowSeconds", "findtime"),
                                   ("banSeconds", "bantime"), ("maxBanSeconds", "bantime.maxtime")]}})
        return result

    def status(self):
        with self.lock:
            healthy = True
            for jail in JAILS.values():
                try:
                    self.command("status", jail)
                except ValueError:
                    healthy = False
            now = int(time.time())
            with self.connect() as db:
                rows = db.execute("SELECT site,ip,expires FROM bans WHERE expires>? ORDER BY expires DESC LIMIT 100", (now,)).fetchall()
                count = db.execute("SELECT count(*) FROM bans WHERE expires>?", (now,)).fetchone()[0]
            return {"available": True, "healthy": healthy, "policies": self.policies(),
                    "bans": [{"site": s, "ip": ip, "expiresAt": expiry} for s, ip, expiry in rows], "totalBans": count}

    def set_policy(self, value):
        value = validate_policy(value)
        with self.lock:
            path = Path(self.config["policy_file"])
            original = path.read_text() if path.exists() else ""
            parser = configparser.ConfigParser(interpolation=None)
            parser.read_string(original)
            jail = JAILS[value["site"]]
            if not parser.has_section(jail):
                parser.add_section(jail)
            for key, field in [("maxFailures", "maxretry"), ("windowSeconds", "findtime"),
                               ("banSeconds", "bantime"), ("maxBanSeconds", "bantime.maxtime")]:
                parser[jail][field] = str(value[key])
            import io
            output = io.StringIO()
            parser.write(output)
            atomic_text(path, output.getvalue())
            try:
                self.command("-t")
                self.command("reload", jail)
                for key, field in [("maxFailures", "maxretry"), ("windowSeconds", "findtime"), ("banSeconds", "bantime")]:
                    if int(self.command("get", jail, field)) != value[key]:
                        raise ValueError("Policy readback failed")
            except Exception:
                atomic_text(path, original)
                self.command("reload", jail)
                raise
            return self.status()

    def unban(self, value):
        if not isinstance(value, dict) or set(value) != {"site", "ip"} or value["site"] not in JAILS:
            raise ValueError("Invalid unban request")
        ip = valid_ip(value["ip"])
        with self.lock:
            self.command("set", JAILS[value["site"]], "unbanip", ip)
            self.change_ban(value["site"], ip, 0)
            return self.status()

    def change_ban(self, site, ip, seconds):
        if site not in JAILS or not 0 <= seconds <= 604800:
            raise ValueError("Invalid ban")
        ip = valid_ip(ip)
        if ipaddress.ip_address(ip).is_loopback or ipaddress.ip_address(ip).is_unspecified:
            raise ValueError("Cannot ban proxy address")
        with self.connect() as db:
            db.execute("DELETE FROM bans WHERE expires<=?", (int(time.time()),))
            if seconds:
                db.execute("INSERT INTO bans VALUES (?,?,?) ON CONFLICT(site,ip) DO UPDATE SET expires=excluded.expires",
                           (site, ip, int(time.time()) + seconds))
            else:
                db.execute("DELETE FROM bans WHERE site=? AND ip=?", (site, ip))

    def check(self, site, ip):
        if site not in JAILS:
            raise ValueError("Unknown site")
        ip = valid_ip(ip)
        now = int(time.time())
        with self.connect() as db:
            row = db.execute("SELECT expires FROM bans WHERE site=? AND ip=?", (site, ip)).fetchone()
        if row and row[0] > now:
            return row[0] - now
        # 廉价短时入口限制也覆盖更换用户名；已封禁来源不消耗全局窗口。
        with self.burst_lock:
            for key in [k for k, (_, expiry) in self.burst.items() if expiry <= now]:
                del self.burst[key]
            key = (site, ip)
            count, expiry = self.burst.get(key, (0, now + 60))
            if count >= 10:
                return max(1, expiry - now)
            global_key = (site, "global")
            total, global_expiry = self.burst.get(global_key, (0, now + 60))
            if total >= 200 or len(self.burst) >= 10000:
                return max(1, global_expiry - now)
            self.burst[key] = (count + 1, expiry)
            self.burst[global_key] = (total + 1, global_expiry)
        return 0


class GuardHandler(BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(3)

    def do_GET(self):
        try:
            if self.path not in ("/check/api", "/check/panel"):
                self.send_error(404)
                return
            values = self.headers.get_all("X-Login-Client-IP", [])
            if len(values) != 1:
                raise ValueError("Missing client address")
            retry = self.server.security.check(self.path.rsplit("/", 1)[1], values[0])
            status = 429 if retry else 204
        except ValueError:
            status, retry = 400, 0
        except (OSError, sqlite3.Error):
            status, retry = 503, 5
        body = json.dumps({"success": False, "error": {"code": "too_many_login_attempts" if status == 429 else "login_protection_unavailable",
                            "message": "登录尝试过于频繁，请稍后再试" if status == 429 else "登录防护暂不可用"},
                            "msg": "登录尝试过于频繁，请稍后再试"}, ensure_ascii=False).encode() if status != 204 else b""
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        if retry:
            self.send_header("Retry-After", str(retry))
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


class GuardServer(socketserver.UnixStreamServer):
    # 内存/CPU 有界；检查只做一次索引查询，socket 读超时 3 秒。
    request_queue_size = 128


def start_guard(security):
    path = Path(security.config["socket"])
    path.unlink(missing_ok=True)
    server = GuardServer(str(path), GuardHandler)
    server.security = security
    os.chown(path, 0, grp.getgrnam(security.config["socket_group"]).gr_gid)
    path.chmod(0o660)
    threading.Thread(target=server.serve_forever, daemon=True, name="login-guard").start()
    return server


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--database", required=True)
    parser.add_argument("action", choices=("ban", "unban"))
    parser.add_argument("site", choices=JAILS)
    parser.add_argument("ip")
    parser.add_argument("seconds", type=int, nargs="?", default=0)
    args = parser.parse_args()
    security = LoginSecurity({"database": args.database})
    security.change_ban(args.site, args.ip, args.seconds if args.action == "ban" else 0)


if __name__ == "__main__":
    main()
