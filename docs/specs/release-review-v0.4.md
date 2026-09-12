# 发布前代码评审记录（v0.4 / 任务：发布前代码评审）

> 评审范围：分层偏序 / 可诊断性 / 安全边界 / 打包可复现。
> 依据：ARCHITECTURE.md §3.2 / §7 / §10 / §9；PRODUCT-DESIGN.md M-4 / M-5 / M-6。
> 结论：**五项评审要点全部通过，无 release blocker。**

## 1. 分层偏序（§3.2）

- `scripts/check-dep-layers.py --workspace .`：**✓ layering OK: 236 workspace crates — all dependency edges respect §3.2 partial order**。
- 覆盖所有新增边（acp/jsonrpc+transport、agent/runner+runtime、tui/encoders+images+run、bridge dispatch 缓存层）。脚本纳入 CI（`ci` job 首步），分层违例可使 CI 失败。**通过。**

## 2. 可诊断性（M-4：零 unknown error）

- 全仓 grep `"unknown error"` / `未知错误`：**0 处**（除"绝不出现 unknown error"的否定性断言/文档）。
- 错误码清单与 `webai_protocol::codes` 模块一致：标准码（PARSE_ERROR −32700 … INTERNAL_ERROR −32603）+ 域码（LOAD_TIMEOUT −32001、PATH_NOT_ALLOWED −32002、PAIRING_* −32010 段），无游离裸码。
- 全 crate thiserror 结构化错误（ScriptError / LoopError / RuntimeError::{ConfigParse,MissingHook} / LlmError / MemoryError）；journey C 测试断言错误含 `code=` 且不含 unknown。**通过（M-4 = 100%）。**

## 3. 安全边界（M-5：0 例穿越/覆盖）

- 路径拼接走查：`JsonlSessionRecorder::validate_session_id`（拒 `..`、`/`、前导 `.`，单测含 `../escape`）、下载 allowlist（PATH_NOT_ALLOWED）、fixture/临时目录均以 `temp_dir + 受控文件名` 组装。**无未校验拼接入口。**
- 网络边界走查：`NetworkPolicy::resolve`（默认 loopback，`--public` 无 pairing 凭据时启动拒绝）+ `admit`（FNV-1a 常时比较，wrong key → `pairing_invalid`）+ transport accept 层 JSON-RPC −32010 段错误。**无绕过入口。**
- **通过（M-5 = 0 例）。**

## 4. 打包可复现（§10）

- 同 commit 两次 `cargo build --release -p webai`：`cmp` 字节一致（**REPRODUCIBLE**）。差异如有（未来引入 env! 时间戳等）须在 PR 中解释——当前无此类来源。
- release 二进制 304,656 字节；legacy_cpp 路径由固定 tag WPE 镜像（`docs/wpe-image.md`）保证环境可复现。**通过。**

## 5. 遗留 TODO 扫描

- `grep TODO|FIXME crates/ bins/`：**0 处**。占位性"lands in a later milestone"注释均为设计内延后且不阻塞 M-7 门禁项（各门禁已有对应实现与测试）。**通过。**

## 评审意见闭环

| 要点 | 结论 | 证据 |
|---|---|---|
| 分层 | 已修（脚本化+CI） | check-dep-layers ✓ 236 crates |
| 零 unknown | 已修（thiserror 全覆盖） | grep 0 处 + journey C |
| 安全边界 | 已修 | 走查 §3 + M-5 单测 |
| 打包可复现 | 已修 | cmp REPRODUCIBLE |
| TODO | 无遗留 | grep 0 |
