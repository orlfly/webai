# Task #69 安全硬化 — 路径沙箱静态审计结论（交付记录）

日期：2026-09-12 · 审计范围：`webai-ng/` 全部 crate · 关联：FR-8 / M-5（release blocker）

## 一、路径拼接点清单与结论

静态扫描 `join(` / `fs::write` / `create_dir_all` / `File::create`，共发现 4 类写入/落盘点：

| # | 位置 | 路径来源 | 风险 | 处置 |
|---|------|---------|------|------|
| 1 | `webai-memory/src/lib.rs` `JsonlSessionRecorder::new_for_dir` | `session_id`（协议侧可控）拼入 `<dir>/{id}.jsonl` | **高**：`../`、绝对路径、`\` 可穿越 | **已修复**：新增 `validate_session_id`（仅允许 `[A-Za-z0-9_-]`，拒绝空/`.`前缀/`..`），构造器先校验后落盘 |
| 2 | `webai-bridge` 下载入口 | `args.filename` → Content-Disposition → URL 尾段（用户可控链） | **高**：穿越/覆盖（M-5） | **已修复**：新增 `download_guard::sanitize_filename`（拒绝分隔符/`..`/隐藏/Windows 保留名），覆盖载荷集单测 |
| 3 | `webai-config` `read_required/read_optional` | `dir.join(固定文件名)`（`agent.toml` 等） | 低：文件名为常量，不可注入 | 无需修改；目录由 `WEBAI_CONFIG`/HOME 决定，属预期行为 |
| 4 | `webai-agent` 会话恢复 `resume_transcript` | CLI `--resume <path>`（本机用户输入） | 低：读取-only，且非网络可达 | 无需修改；写侧由 `JsonlSessionRecorder` 守护 |

**结论：写入路径拼接 100% 经校验函数**（#1、#2 已修，#3、#4 为常量/只读豁免）。

## 二、新增防线与测试

| 防线 | 测试 | 结果 |
|------|------|------|
| session_id 穿越 payload 集（10 例：`../`、`\`、绝对路径、隐藏、空、多级穿越） | `webai-memory::tests::session_id_traversal_payloads_are_rejected` | 全部拒绝 ✓ |
| recorder 构造器拒绝越界建文件 | `recorder_never_creates_file_outside_dir` | ✓ |
| 下载文件名穿越/覆盖 payload 集（12 例：绝对路径、`..`、分隔符、`CON`/`NUL` 保留名） | `webai-bridge::download_guard::tests::traversal_payload_set_is_rejected` | 全部拒绝 ✓ |
| 良性文件名不误伤 | `benign_filenames_pass` | ✓ |
| 网络边界：默认 loopback、非 loopback 拒绝 | `webai-acp::net::tests::default_policy_binds_loopback_and_refuses_remote` | ✓ |
| `--public` 未配对启动即拒 | `public_without_pairing_is_startup_error` | ✓ |
| `--public` 每请求配对校验（未配对拒、配对过、loopback 也需配对） | `public_mode_requires_pairing_per_request` | ✓ |
| 护栏不可经自然语言关闭（与 M4-5 联合断言） | `policy_cannot_be_disabled_by_natural_language` + plan_loop 同名测试 | ✓ |

M-5 沙箱逃逸（绝对路径 / `..` / symlink 指向外部）端到端 0 例——`webai-agent` sandbox 模块
（#54，PR #92）已有同等 payload 覆盖；本任务补齐 memory / download 两个入口。

## 三、网络边界默认值

- 默认绑定 `127.0.0.1`（`net::DEFAULT_BIND`），非 loopback 源在准入层被拒（结构化 `non_loopback_refused`）。
- `--public` 绑定 `0.0.0.0` 且**必须**先有配对凭据，否则启动失败（`PublicWithoutPairing`）；运行期每请求校验（`pairing_required`）。

## 四、残留事项

- 配对握手协议本体（密钥交换/轮换）按里程碑划分至 M6-5；本任务交付「强制配对」的准入与启动 gate。
- `webai-config` 目录解析为只读消费用户配置，遵循最小权限原则不改动。