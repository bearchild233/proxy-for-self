"""按 Git 变更范围选择 CI；无法判断时执行全部检查。"""
import json
import os
from pathlib import Path
import subprocess


def classify(paths):
    scopes = set()
    for path in paths:
        if path.startswith("backend/"):
            scopes.add("backend")
        elif path.startswith(("frontend/", "packages/ui-kit/", "packages/plugin-sdk/")) or path == "scripts/build-plugin.mjs":
            scopes.add("frontend")
            if path.startswith("packages/plugin-sdk/"):
                scopes.add("worker")
        elif path.startswith("plugins/") and ("/ui/" in path or path.endswith("/plugin.json")):
            scopes.add("frontend")
        elif path.startswith("plugins/"):
            scopes.add("worker")
        elif path.startswith(("plugins/", "services/", "tools/")):
            scopes.add("worker")
        elif path.startswith("docs/") or path in (
            "README.md", "AGENTS.md", "CONTRIBUTING.md", "LICENSE", "NOTICE",
        ):
            continue
        else:
            return {"backend", "frontend", "worker"}
    return scopes


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    name = os.environ["GITHUB_EVENT_NAME"]
    scopes = {"backend", "frontend", "worker"}
    if name in ("push", "pull_request"):
        if name == "pull_request":
            base = event["pull_request"]["base"]["sha"]
            head = event["pull_request"]["head"]["sha"]
            revisions = f"{base}...{head}"
        else:
            revisions = f"{event['before']}..{os.environ['GITHUB_SHA']}"
        try:
            paths = subprocess.check_output(
                ["git", "diff", "--name-only", "--no-renames", "-z", revisions, "--"],
                stderr=subprocess.DEVNULL,
            ).decode().split("\0")
            scopes = classify(path for path in paths if path)
        except subprocess.CalledProcessError:
            pass
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        for scope in ("backend", "frontend", "worker"):
            output.write(f"{scope}={str(scope in scopes).lower()}\n")


if __name__ == "__main__":
    main()
