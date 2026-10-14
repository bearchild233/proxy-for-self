#!/usr/bin/env python3
"""模块文档检查：覆盖、失效引用、过时检测。

纯标准库，Windows Python 与 WSL 均可运行。从仓库任意位置执行：

    python scripts/check-doc-map.py            # 覆盖与引用失败返回 1，过时只列出
    python scripts/check-doc-map.py --strict   # 过时文档也返回 1（阶段收尾使用）

文档约定见 docs/modules.md 与 CONTRIBUTING.md“文档维护”一节。
"""
import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# ---------------------------------------------------------------------------
# 规则常量
# ---------------------------------------------------------------------------

# 总表与模块文档所在位置。
INDEX_DOC = 'docs/modules.md'
MODULE_DOC_DIR = 'docs/modules'
# 其余带 doc-sync 首行注释的文档（如 docs/gateway-pool.md）放在 docs/ 一级，按首行标记自动识别。
EXTRA_DOC_DIR = 'docs'

# 受管目录：其一级子目录必须被文档提到。
MANAGED_DIRS = [
    'backend/apps',
    'backend/crates',
    'services',
    'plugins',
    'packages',
    'frontend/src',
    'scripts',
    'deploy',
    'tools',
]
# 这些目录只作为容器，要求覆盖它们的下一级（backend/crates/providers/*）。
CONTAINER_DIRS = {'backend/crates/providers'}
# 这些目录下的源文件（直接位于该目录或其一级子目录 services/*/ 中）也必须被提到。
FILE_COVERED_DIRS = ['scripts', 'tools', 'deploy']
FILE_COVERED_GLOB_DIRS = ['services']  # services/*/ 下的直接文件
SOURCE_EXT = {'.py', '.rs', '.sh', '.ps1', '.mjs', '.js', '.ts'}
# deploy 下的直接文件（Dockerfile、compose.yaml 等）也算：不看扩展名，README 除外。
ANY_FILE_DIRS = {'deploy'}

# 排除：测试、构建产物、缓存、生成文件。
EXCLUDED_DIR_NAMES = {'tests', 'test', 'plugin-tests', '__pycache__', 'node_modules',
                      'target', 'dist', 'build', '.venv', 'venv', 'fixtures'}
EXCLUDED_FILE_SUFFIXES = ('.lock', '.pyc', '.log')
EXCLUDED_FILE_NAMES = {'Cargo.lock', 'pnpm-lock.yaml', 'package-lock.json', 'README.md'}
TEST_FILE_PATTERNS = [
    re.compile(r'(^|/)test_[^/]*\.py$'),
    re.compile(r'[^/]*_test\.py$'),
    re.compile(r'[^/]*\.(test|spec)\.(mjs|js|ts|tsx)$'),
]

# 反引号里“看起来是仓库路径”时才检查；第一段属于这些根目录之一就按仓库根解析。
KNOWN_ROOTS = {'backend', 'frontend', 'services', 'plugins', 'packages', 'scripts', 'deploy',
               'docs', 'tools', 'release', 'licenses', '.github'}
# 不带根目录的相对路径或裸文件名，只在扩展名属于下列集合时才要求存在。
CODE_EXT = {'.rs', '.py', '.ts', '.vue', '.mjs', '.sh', '.ps1', '.tsx', '.js'}
PATH_TOKEN = re.compile(r'^[A-Za-z0-9_.@+-]+(/[A-Za-z0-9_.@+-]+)*$')
# 含这些字符的反引号内容视为命令、模板或通配，不是路径。
NON_PATH_CHARS = set(' <>*{}$|()[]=,;"\'?!#%^&`\\')

# 过时检测不关心的改动：文档、锁文件。
STALE_IGNORED_SUFFIXES = ('.md', '.lock')
STALE_IGNORED_NAMES = {'Cargo.lock', 'pnpm-lock.yaml', 'package-lock.json'}

MARKER = re.compile(r'<!--\s*doc-sync paths:\s*(.*?)\s*-->', re.S)
REVIEWED = re.compile(r'reviewed:\s*([0-9a-fA-F]{7,40})')


# ---------------------------------------------------------------------------
# 基础工具
# ---------------------------------------------------------------------------

def git(*args, check=False):
    """运行 git，返回 stdout 文本；失败返回空串。"""
    result = subprocess.run(['git', '-C', str(ROOT), *args], capture_output=True, text=True,
                            encoding='utf-8', errors='replace')
    if check and result.returncode:
        raise RuntimeError('git ' + ' '.join(args) + ' 失败')
    return result.stdout if result.returncode == 0 else ''


def repo_files():
    """已跟踪与未跟踪且未忽略的文件（相对仓库根，正斜杠）；无 git 时遍历目录。"""
    out = git('ls-files', '-co', '--exclude-standard')
    if out:
        return sorted(set(line for line in out.splitlines() if line and (ROOT / line).exists()))
    files = []
    for base, dirs, names in os.walk(ROOT):
        dirs[:] = [d for d in dirs if not d.startswith('.') and d not in EXCLUDED_DIR_NAMES]
        for name in names:
            files.append(os.path.relpath(os.path.join(base, name), ROOT).replace('\\', '/'))
    return sorted(files)


def is_test_path(path):
    parts = path.split('/')
    if any(part in EXCLUDED_DIR_NAMES for part in parts[:-1]):
        return True
    return any(pattern.search(path) for pattern in TEST_FILE_PATTERNS)


def excluded(path):
    name = path.rsplit('/', 1)[-1]
    if is_test_path(path) or name in EXCLUDED_FILE_NAMES or name.endswith(EXCLUDED_FILE_SUFFIXES):
        return True
    return any(part.startswith('.') for part in path.split('/'))


def mentioned(path, text):
    """路径字符串是否出现在文本里：后面不能紧跟路径字符（避免 plugins/sdk 命中 plugins/sdk-x）。"""
    return re.search(r'(?<![\w./-])' + re.escape(path) + r'(?![\w-]|\.\w)', text) is not None


# ---------------------------------------------------------------------------
# 文档加载
# ---------------------------------------------------------------------------

class Doc:
    def __init__(self, rel):
        self.rel = rel
        self.text = (ROOT / rel).read_text(encoding='utf-8')
        head = '\n'.join(self.text.splitlines()[:12])
        match = MARKER.search(head)
        self.paths = []
        if match:
            # reviewed 可写在同一注释里（reviewed: abc1234）；路径列表到它为止。
            self.paths = match.group(1).split('reviewed:', 1)[0].split()
        reviewed = REVIEWED.search(head)
        self.reviewed = reviewed.group(1) if reviewed else None


def load_docs():
    docs = []
    index = ROOT / INDEX_DOC
    if index.exists():
        docs.append(Doc(INDEX_DOC))
    module_dir = ROOT / MODULE_DOC_DIR
    if module_dir.is_dir():
        for path in sorted(module_dir.glob('*.md')):
            docs.append(Doc(f'{MODULE_DOC_DIR}/{path.name}'))
    for path in sorted((ROOT / EXTRA_DOC_DIR).glob('*.md')):
        rel = f'{EXTRA_DOC_DIR}/{path.name}'
        if rel == INDEX_DOC:
            continue
        doc = Doc(rel)
        if doc.paths:  # 只收带 doc-sync 标记的规则文档
            docs.append(doc)
    return docs


# ---------------------------------------------------------------------------
# 1. 覆盖检查
# ---------------------------------------------------------------------------

def required_paths(files):
    """受管目录下必须被文档提到的路径集合。"""
    required = set()
    for managed in MANAGED_DIRS:
        prefix = managed + '/'
        children = set()
        for path in files:
            if not path.startswith(prefix) or excluded(path):
                continue
            rest = path[len(prefix):]
            if '/' in rest:
                children.add(rest.split('/', 1)[0])
        for child in children:
            full = prefix + child
            if full in CONTAINER_DIRS:
                for path in files:
                    if path.startswith(full + '/') and not excluded(path):
                        required.add(full + '/' + path[len(full) + 1:].split('/', 1)[0])
            else:
                required.add(full)
    for base in FILE_COVERED_DIRS:
        for path in files:
            if not path.startswith(base + '/') or excluded(path) or '/' in path[len(base) + 1:]:
                continue
            if base in ANY_FILE_DIRS or Path(path).suffix in SOURCE_EXT:
                required.add(path)
    for base in FILE_COVERED_GLOB_DIRS:
        for path in files:
            rest = path[len(base) + 1:] if path.startswith(base + '/') else ''
            if rest.count('/') == 1 and not excluded(path) and Path(path).suffix in SOURCE_EXT:
                required.add(path)
    return required


def check_coverage(files, docs):
    corpus = '\n'.join(doc.text for doc in docs)
    return sorted(path for path in required_paths(files) if not mentioned(path, corpus))


# ---------------------------------------------------------------------------
# 2. 失效引用
# ---------------------------------------------------------------------------

def strip_fences(text):
    """去掉围栏代码块，命令示例里的路径不检查。"""
    return re.sub(r'^```.*?^```', '', text, flags=re.S | re.M)


def inline_tokens(text):
    return re.findall(r'`([^`\n]+)`', strip_fences(text))


def normalize_token(token):
    token = token.strip()
    token = token.split('::', 1)[0]
    token = re.sub(r':\d+(-\d+)?$', '', token)
    return token.rstrip('/')


def relative_paths(files, bases):
    """doc-sync 路径之下所有文件与目录的“尾部子路径”集合，用于解析文档里省略前缀的相对路径。"""
    found = set()
    for path in files:
        if not any(path == base or path.startswith(base + '/') for base in bases):
            continue
        parts = path.split('/')
        for start in range(len(parts)):
            found.add('/'.join(parts[start:]))
            for end in range(start + 1, len(parts)):
                found.add('/'.join(parts[start:end]))
    return found


def check_references(docs, files):
    names = {}
    for path in files:
        names.setdefault(path.rsplit('/', 1)[-1], []).append(path)
    problems = []
    for doc in docs:
        bases = [p.rstrip('/') for p in doc.paths]
        for path in doc.paths:
            if not (ROOT / path.rstrip('/')).exists():
                problems.append((doc.rel, f'doc-sync 路径不存在: {path}'))
        local = relative_paths(files, bases)
        seen = set()
        for raw in inline_tokens(doc.text):
            token = normalize_token(raw)
            if not token or token in seen or NON_PATH_CHARS & set(token) or '...' in token or '://' in token:
                continue
            if token.startswith(('.', '/', '-', '~')) and not token.startswith('.github'):
                continue
            if not PATH_TOKEN.match(token):
                continue
            seen.add(token)
            suffix = Path(token).suffix
            first = token.split('/', 1)[0]
            if '/' in token:
                if first in KNOWN_ROOTS:
                    if not (ROOT / token).exists():
                        problems.append((doc.rel, token))
                    continue
                # 相对路径按 doc-sync 路径下的尾部子路径解析（文档里常省略 src/、ui/ 等前缀）。
                if token not in local and (first in local or suffix in CODE_EXT):
                    problems.append((doc.rel, token))
            elif suffix in CODE_EXT:
                candidates = names.get(token, [])
                if doc.rel == INDEX_DOC or not bases:
                    ok = bool(candidates)
                else:
                    ok = any(c.startswith(base + '/') for c in candidates for base in bases)
                if not ok:
                    problems.append((doc.rel, token))
    return problems


# ---------------------------------------------------------------------------
# 3. 过时检测
# ---------------------------------------------------------------------------

def commit_of(rev):
    out = git('rev-parse', '--verify', '--quiet', rev + '^{commit}').strip()
    return out or None


def newer(a, b):
    """返回 a、b 中较新的提交；彼此无祖先关系时取 a。"""
    if not a or not b or a == b:
        return a or b
    if subprocess.run(['git', '-C', str(ROOT), 'merge-base', '--is-ancestor', a, b]).returncode == 0:
        return b
    return a


def relevant(path):
    name = path.rsplit('/', 1)[-1]
    return not (is_test_path(path) or path.endswith(STALE_IGNORED_SUFFIXES) or name in STALE_IGNORED_NAMES)


def stale_docs(docs):
    result = []
    for doc in docs:
        if not doc.paths:
            continue
        last = git('log', '-1', '--format=%H', '--', doc.rel).strip() or None
        reviewed = commit_of(doc.reviewed) if doc.reviewed else None
        start = newer(last, reviewed)
        committed = []
        if start:
            out = git('diff', '--name-only', start, 'HEAD', '--', *doc.paths)
            committed = sorted(p for p in out.splitlines() if p and relevant(p))
        workspace = []
        out = git('status', '--porcelain', '--untracked-files=all', '--', *doc.paths)
        for line in out.splitlines():
            entry = line[3:].split(' -> ')[-1].strip().strip('"')
            if entry and relevant(entry):
                workspace.append(entry)
        if committed or workspace:
            result.append((doc, start, committed, sorted(set(workspace))))
    return result


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------

def summarize(items, limit=6):
    shown = ', '.join(items[:limit])
    return shown + (f' 等共 {len(items)} 个' if len(items) > limit else '')


def main():
    if hasattr(sys.stdout, 'reconfigure'):
        sys.stdout.reconfigure(encoding='utf-8')
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--strict', action='store_true', help='过时文档也算失败（阶段收尾使用）')
    args = parser.parse_args()

    files = repo_files()
    docs = load_docs()
    if not any(doc.rel == INDEX_DOC for doc in docs):
        print(f'缺少总表 {INDEX_DOC}')
        return 1

    missing = check_coverage(files, docs)
    broken = check_references(docs, files)
    stale = stale_docs(docs)

    for path in missing:
        print(f'覆盖缺失: {path}')
    for doc, token in broken:
        print(f'失效引用: {doc}: {token}')
    for doc, start, committed, workspace in stale:
        origin = start[:8] if start else '未提交'
        if committed:
            print(f'待更新: {doc.rel} (起点 {origin}) 已提交改动: {summarize(committed)}')
        if workspace:
            print(f'待更新(工作区): {doc.rel} 未提交改动: {summarize(workspace)}')
    print(f'覆盖 {len(missing)} 缺失，{len(broken)} 失效引用，{len(stale)} 个文档待更新')

    if missing or broken or (args.strict and stale):
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
