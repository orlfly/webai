# Task #39 M6 里程碑 — 风险登记表（§9 每条均可执行验证）

日期：2026-09-12 · 关联：PRODUCT-DESIGN §9 · 原则：**不允许只列不验**——每条风险都绑定一个可执行的验证（测试名 / 命令）。

| # | 风险 | 可能性 | 影响 | 可执行验证 | 状态 |
|---|------|--------|------|-----------|------|
| R1 | LLM token 成本失控（重复 compose / 无记忆复用） | 中 | 高 | `cargo test -p webai-bridge --test perf_compare`：200 步 / 10 url → 95% 命中率、190 hits；`journey_b_repeat_marks_reused_script` 断言第二次 `reused_script=true` | 已验证 ✓ |
| R2 | 循环死循环 / 卡死（同观察反复） | 中 | 高 | `webai-agent` plan_loop：`duplicate_observation_guard_triggers`、`max_steps_guard_triggers_after_budget`（护栏默认开，`guard_cannot_be_disabled_by_natural_language` 禁话术关闭） | 已验证 ✓ |
| R3 | 路径穿越 / 覆盖已有文件（M-5 release blocker） | 低 | 极高 | `webai-memory::session_id_traversal_payloads_are_rejected`（10 载荷全拒）、`webai-bridge::download_guard::traversal_payload_set_is_rejected`（12 载荷全拒）、`webai-agent` sandbox 逃逸集 0 例；审计记录 `docs/security-audit-69.md` | 已验证 ✓ |
| R4 | 远程接入被恶意利用（未配对公网） | 低 | 极高 | `webai-acp::net::default_policy_binds_loopback_and_refuses_remote`（非 loopback 拒）、`public_without_pairing_is_startup_error`（启动即拒）、`public_mode_requires_pairing_per_request`（每请求校验）；e2e `release_gate_network_boundary` | 已验证 ✓ |
| R5 | 崩溃后丢会话（kill -9 / 截断） | 中 | 高 | `webai-agent` runtime：`crash_recovery_fuzz_truncation_at_any_offset`（逐字节截断全通过）、`crash_recovery_skips_midfile_corruption`；e2e `journey_d_crash_resume_rebuilds_transcript` | 已验证 ✓ |
| R6 | 部署缺外部脚本 / 系统依赖 | 中 | 中 | `webai-webkit::bundle::self_check_is_filesystem_independent`（无文件系统访问）、`embedded_set_matches_declared_order_exactly`（15 模块顺序一致）；实测：release 二进制（305KB strip）拷入干净目录可运行 | 已验证 ✓ |
| R7 | 非法配置导致半启动态 | 中 | 中 | `webai-agent` runtime：`bootstrap_fail_fast_on_missing_agent_toml` / `..._missing_llm_toml` / `..._unknown_llm_profile`；e2e `release_gate_modes_and_fail_fast` | 已验证 ✓ |
| R8 | 记忆后端不可用打断主流程 | 中 | 中 | `webai-memory::disabled_store_rejects_writes_and_recalls_nothing`（降级不 panic）；runtime `bootstrap_degrades_without_optional_files`（无 mem.toml 主流程可装配） | 已验证 ✓ |
| R9 | 协议破坏既有 ACP 方法面（迁移 §11） | 低 | 高 | e2e `release_gate_unknown_method_is_structured`（-32601 结构化）；`session/prompt`、`session/close`、`ping`、`agent/health` 兼容回归在 webai-acp 单测 | 已验证 ✓ |
| R10 | 前端越层触碰 bridge/webkit（§2 边界） | 低 | 中 | webai-tui Cargo.toml 仅依赖 webai-agent / webai-protocol（无 bridge/webkit 依赖边）；session 后台服务以 mpsc 隔离 | 已验证 ✓ |

## 里程碑验收总表对照（§6 M-1~M-7）

| 指标 | 目标 | 实测 | 验证 |
|------|------|------|------|
| M-1 用例矩阵通过率 | ≥95% | 26/26 套件全绿（含 e2e 7 例 + 旅程 A/B/C/D） | `cargo test --workspace` |
| M-2 调用数比 | ≤30%（复用后） | 5%（190/200 命中） | `perf_compare` |
| M-3 崩溃恢复成功率 | 100% | 逐字节截断 fuzz 全通过 | `crash_recovery_fuzz_*` |
| M-4 结构化失败占比 | 100% | 错误均带 `code=` / 错误枚举；`journey_c_failure_is_structured` 断言无 "unknown error" | `journey_c_*` |
| M-5 穿越/覆盖 | 0 例 | 22 个载荷全拒 | memory/bridge payload 测试 |
| M-6 单会话空闲内存 | <300MB | stub 阶段进程远低于阈值（release 二进制 305KB；FFI 接入后按 §6.1 view 池预算复核，属 M6-5 遗留测量点） | 部署自检 |
| M-7 打包体积/优化 | 记录在案 | release 305,656 bytes（opt-level 3 + LTO + codegen-units 1 + strip） | `cargo build --release` |

## 遗留（登记在案 + 原因）

- **M-6 真实 WebKit 内存测量**：当前 FFI 未接入（stub），300MB 预算的实测需 `legacy_cpp`/cog 环境到位（PR #78 系列）；release 门禁以 stub 路径验证，FFI 路径在合并后由 devops 环境复测。
- **M-1 未达标用例**：无（矩阵内用例全部通过；矩阵覆盖旅程 A/B/C/D 与全部 release gate）。