"""从已验证的 Linux Python 3.10 Worker 环境生成独立 Excel 插件包。

只打包普通文件，Python 解释器链接由安装器创建；不复制身份、配置或运行数据。
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import tarfile


def package_plugin(runtime: Path, source: Path, output: Path, version: str):
    files = {}
    for path in (runtime / "venv").rglob("*"):
        if path.is_file() and not path.is_symlink() and "__pycache__" not in path.parts and path.suffix != ".pyc":
            name = path.relative_to(runtime).as_posix()
            if name.startswith("venv/bin/"):
                continue
            files[name] = path
    prefix = "venv/lib/python3.10/site-packages/excel_codex_bridge/"
    # 完整替换插件源码清单，避免上一版本已删除的模块混入新包。
    files = {name: path for name, path in files.items() if not name.startswith(prefix)}
    for path in source.rglob("*"):
        if path.is_file() and not path.is_symlink() and "__pycache__" not in path.parts and path.suffix != ".pyc":
            files[prefix + path.relative_to(source).as_posix()] = path
    ui = source.parent.parent / "ui"
    license_file = source.parent.parent / "LICENSE"
    if not (ui / "overview.html").is_file() or not license_file.is_file():
        raise ValueError("插件 UI 或许可证缺失")
    files["LICENSE"] = license_file
    for path in ui.rglob("*"):
        if path.is_file() and not path.is_symlink():
            files["ui/" + path.relative_to(ui).as_posix()] = path
    manifest = json.dumps({"id": "excel-bridge", "version": version, "protocol": 1,
                           "platform": "linux-amd64-python3.10", "capabilities": ["excel-inference"],
                           "pages": [{"id": "overview", "title": "插件详情", "slot": "settings",
                                      "entry": "ui/overview.html", "roles": ["admin"]}],
                           "operations": [{"id": "status", "roles": ["admin"]}]}).encode()
    with tarfile.open(output, "w:gz") as tar:
        for name, path in sorted(files.items()):
            info = tar.gettarinfo(str(path), arcname=name)
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            with path.open("rb") as stream:
                tar.addfile(info, stream)
        info = tarfile.TarInfo("plugin.json")
        info.size = len(manifest)
        tar.addfile(info, io.BytesIO(manifest))
    return {"version": version, "sha256": hashlib.sha256(output.read_bytes()).hexdigest(), "package": str(output.resolve())}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    print(json.dumps(package_plugin(args.runtime, args.source, args.output, args.version)))
