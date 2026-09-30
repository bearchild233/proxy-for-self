#!/usr/bin/env python3
"""Run the Worker checks in a disposable environment, identical locally and in CI."""
from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main() -> None:
    if sys.version_info < (3, 10):
        raise SystemExit("Python 3.10 or newer is required")
    project = Path(__file__).resolve().parents[1] / "plugins" / "excel-bridge"
    env = {
        key: value for key, value in os.environ.items()
        if key not in {"PYTHONPATH", "PYTHONHOME", "PYTHONUSERBASE", "VIRTUAL_ENV"}
        and not key.startswith("PYTEST_")
    }
    env["PYTEST_DISABLE_PLUGIN_AUTOLOAD"] = "1"
    env["PYTHONNOUSERSITE"] = "1"
    with tempfile.TemporaryDirectory(prefix="proxy-excel-test-") as directory:
        root = Path(directory)
        venv = root / "venv"
        subprocess.run([sys.executable, "-I", "-m", "venv", str(venv)], check=True, env=env)
        python = venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")

        def run(*args: str) -> None:
            subprocess.run([str(python), "-I", *args], cwd=root, env=env, check=True)

        # 不让 pip 自动补全漏锁的依赖；pip check 会明确指出缺失或冲突。
        run("-m", "pip", "--isolated", "install", "--no-deps", "-r", str(project / "requirements-test.lock"))
        run("-m", "pip", "--isolated", "install", "--no-deps", "--no-build-isolation", str(project))
        run("-m", "pip", "check")
        # 从安装包导入，禁止源码路径、用户 site-packages 或自动加载的 pytest 插件兜底。
        run("-c", "import sys, pathlib, excel_codex_bridge; "
            "assert sys.prefix != sys.base_prefix; "
            "assert pathlib.Path(excel_codex_bridge.__file__).is_relative_to(sys.prefix)")
        run("-m", "pytest", str(project / "tests"), "-o", "pythonpath=", "-q", *sys.argv[1:])


if __name__ == "__main__":
    main()
