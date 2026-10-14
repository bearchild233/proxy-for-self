# 与 Python 实现的一致性对照

本目录的脚本用 Python 实现（`services/plugin_host/gateway_pool.py` 及其测试桩）驱动动作序列，记录期望输出，
再由 Rust 插件的两个默认忽略的测试重放并逐项比较。**它们依赖 `services/plugin_host` 的 Python 实现；
P2 删除 Python 实现时，本目录与下面两个测试一并删除。**

| 脚本 | 对应的 Rust 测试 | 覆盖 |
| --- | --- | --- |
| `gen_python.py` | `tests/parity.rs` | 存储、设置校验、队列与管理读取（008） |
| `gen_runtime.py` | `tests/runtime_parity.rs` | 运行状态机 `Store::runtime`（009） |

## 用法

需要 Python 3.10（与线上一致）。产物目录放在 `.build/` 下（git 忽略）：

```sh
python3.10 plugins/gateway-pool/service/tests/parity/gen_runtime.py generate . .build/gateway-pool-parity/runtime
CPR_GP_PARITY_DIR=$PWD/.build/gateway-pool-parity/runtime \
  scripts/build-plugins-local.sh "$PWD" 4096 3 test gateway-pool --test runtime_parity -- --ignored
python3.10 plugins/gateway-pool/service/tests/parity/gen_runtime.py verify-rust . .build/gateway-pool-parity/runtime
```

`gen_python.py` 用法相同（对应 `--test parity`）。第一步用 Python 写出期望，第二步 Rust 重放并写出自己的数据库，
第三步 Python 读取 Rust 写出的数据库再比较一次。请求路径钩子（010）不在对照范围内：
钩子是新增的编排，没有 Python 对应物，其行为由 `tests/hooks_*.rs` 和内核侧端到端测试覆盖。
