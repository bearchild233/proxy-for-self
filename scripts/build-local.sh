#!/usr/bin/env bash
set -euo pipefail

src=${1:?repository path required}
memory_mib=${2:?memory budget required}
jobs=${3:?job count required}
task=${4:?task required}
package=${5:?package required}
shift 5
[[ $memory_mib =~ ^[0-9]+$ && $jobs =~ ^[1-3]$ ]] || exit 2
case "$task" in check|test|clippy|build|release) ;; *) exit 2 ;; esac
[[ -f "$src/backend/Cargo.toml" ]] || exit 2
id builder >/dev/null

# 同一 Linux 工作副本串行同步和编译，避免两个终端同时覆盖源码。
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

# 只同步本地构建镜像；缓存留在 Linux 文件系统内，不进入 Windows 仓库。
mkdir -p /work/proxy-for-self/backend
rsync -a --delete --exclude /target --chown=builder:builder "$src/backend/" /work/proxy-for-self/backend/
cd /work/proxy-for-self/backend
export PATH=/home/builder/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export CARGO_TARGET_DIR=/work/proxy-for-self/backend/target
export CARGO_BUILD_JOBS="$jobs"
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=1
export RUST_MIN_STACK=16777216
if [[ $task == release ]]; then
    # 本地发布构建使用较快的代码生成；正式发布仍由标准发布流程完成。
    export CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8
    CPR_GIT_SHA=$(git -C "$src" rev-parse HEAD)
    CPR_BUILD_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    export CPR_GIT_SHA CPR_BUILD_TIME CPR_BUILD_TYPE=source-local
    command=(cargo build --release --locked -p "$package")
else
    command=(cargo "$task" --locked -p "$package")
fi
start=$SECONDS
runuser -u builder -- "${command[@]}" "$@"
echo "Local $task completed in $((SECONDS - start))s; target: $CARGO_TARGET_DIR"
