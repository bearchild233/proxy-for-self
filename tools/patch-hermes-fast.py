"""为明确指定的自有 Responses 代理启用 Hermes /fast 转发，不改变普通请求。

默认只验证并显示差异；--apply 原子更新指定 Hermes 源文件。升级 Hermes 后可重复执行。
"""
import argparse
import ast
import difflib
import os
from pathlib import Path
import tempfile
from urllib.parse import urlsplit

START = "    # proxy-for-self: approved fast endpoint begin\n"
END = "    # proxy-for-self: approved fast endpoint end\n"


def patched(source, base_url):
    url = urlsplit(base_url)
    if url.scheme != "https" or not url.hostname or url.username or url.password or url.query or url.fragment:
        raise ValueError("base-url 必须是无凭据的 HTTPS 服务地址")
    endpoint = (url.scheme, url.hostname.lower(), url.port or 443, url.path.rstrip("/"))
    tree = ast.parse(source)
    function = next((node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "_fast_mode_route_supported"), None)
    if function is None or [arg.arg for arg in function.args.args] != ["model_id", "provider", "base_url"]:
        raise ValueError("Hermes Fast 接口已变化，拒绝自动修改")
    lines = source.splitlines(keepends=True)
    current = "".join(lines[function.lineno - 1:function.end_lineno])
    if START in current:
        start = current.index(START)
        stop = current.index(END, start) + len(END)
        current = current[:start] + current[stop:]
    marker = "    from urllib.parse import urlparse\n"
    if current.count(marker) != 1:
        raise ValueError("Hermes 路由检查结构已变化，拒绝自动修改")
    code = START + f'''    from urllib.parse import urlsplit as _proxy_urlsplit
    try:
        _proxy_url = _proxy_urlsplit(str(base_url or ""))
        _proxy_endpoint = (_proxy_url.scheme, (_proxy_url.hostname or "").lower(), _proxy_url.port or 443, _proxy_url.path.rstrip("/"))
        if (_proxy_endpoint == {endpoint!r}
                and not _proxy_url.username and not _proxy_url.password
                and not _proxy_url.query and not _proxy_url.fragment
                and _is_openai_fast_model(model_id)):
            return True
    except ValueError:
        return False
''' + END
    replacement = current.replace(marker, marker + code)
    result = "".join(lines[:function.lineno - 1]) + replacement + "".join(lines[function.end_lineno:])
    ast.parse(result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="Hermes hermes_cli/models.py")
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    path = args.source.resolve(strict=True)
    before = path.read_text(encoding="utf-8")
    after = patched(before, args.base_url)
    if before == after:
        print("Hermes Fast 支持已配置")
        return
    if not args.apply:
        print("".join(difflib.unified_diff(before.splitlines(True), after.splitlines(True), fromfile=str(path), tofile=str(path))))
        return
    stat = path.stat()
    fd, name = tempfile.mkstemp(prefix=".fast-", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as stream:
            stream.write(after)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(name, stat.st_mode)
        if hasattr(os, "chown"):
            os.chown(name, stat.st_uid, stat.st_gid)
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)
    print("Hermes Fast 转发已修复；现有进程需在空闲后重新加载")


if __name__ == "__main__":
    main()
