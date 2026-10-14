#!/usr/bin/env bash
set -euo pipefail

src=${1:?repository path required}
memory_mib=${2:?memory budget required}
jobs=${3:?job count required}
task=${4:?task required}
shift 4
[[ $memory_mib =~ ^[0-9]+$ && $jobs =~ ^[1-3]$ ]] || exit 2
case "$task" in check|test|clippy|build|release) ;; *) exit 2 ;; esac
[[ -f "$src/plugins/Cargo.toml" ]] || exit 2
id builder >/dev/null

# 与内核共用锁和 cgroup，两个入口不得同时同步或消耗构建预算。
mkdir -p /work/proxy-for-self
exec 9>/work/proxy-for-self/build.lock
flock -n 9 || { echo 'Another local build is running.' >&2; exit 1; }
echo '+memory +cpu +pids' > /sys/fs/cgroup/cgroup.subtree_control
group=/sys/fs/cgroup/proxy-for-self-build
mkdir -p "$group"
echo $((memory_mib * 1024 * 1024)) > "$group/memory.max"
echo 536870912 > "$group/memory.swap.max"
echo '400000 100000' > "$group/cpu.max"
echo 256 > "$group/pids.max"
echo $$ > "$group/cgroup.procs"

# 协议 crate 继承 backend workspace 字段；保留其完整 workspace 元数据。
# backend 镜像复用既有入口，但不构建其余 backend 成员。
mkdir -p /work/proxy-for-self/backend /work/proxy-for-self/plugins /work/proxy-for-self/plugins-target
rsync -a --delete --exclude /target --chown=builder:builder "$src/backend/" /work/proxy-for-self/backend/
rsync -a --delete --exclude target --exclude node_modules \
    --include /Cargo.toml --include /Cargo.lock --include /sdk/ --include '/sdk/***' --exclude '*' \
    --chown=builder:builder "$src/plugins/" /work/proxy-for-self/plugins/
chown builder:builder /work/proxy-for-self/plugins-target
cd /work/proxy-for-self/plugins
export PATH=/home/builder/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export CARGO_TARGET_DIR=/work/proxy-for-self/plugins-target CARGO_BUILD_JOBS="$jobs"
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=1
export RUST_MIN_STACK=16777216
if [[ $task == release ]]; then
    # 插件增量发布测量保留增量缓存；优化配置沿用本地构建入口。
    export CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8
    CPR_GIT_SHA=${CPR_GIT_SHA:-$(git -C "$src" rev-parse HEAD)}
    [[ $CPR_GIT_SHA =~ ^[0-9a-f]{40}$ ]] || exit 2
    export CPR_GIT_SHA
    command=(cargo build --release --locked)
else
    command=(cargo "$task" --locked)
fi
packages=()
while [[ $# -gt 0 && $1 != -* ]]; do packages+=(-p "$1"); shift; done
if [[ ${#packages[@]} == 0 ]]; then packages=(--workspace); fi
start=$SECONDS
runuser -u builder -- "${command[@]}" "${packages[@]}" "$@"
echo "Local $task completed in $((SECONDS - start))s; target: $CARGO_TARGET_DIR"
