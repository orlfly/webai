# webai-tui 交互设计稿

> 任务：TUI 交互设计稿（布局、键位映射与图像降级规范）
> 依据：`docs/architecture/ARCHITECTURE.md` §4.11 webai-tui、`docs/specs/PRODUCT-DESIGN.md` §3.1 TUI 形态 / §4 FR-2 / §5 旅程 A/B/C/D。
> 状态：设计定稿（M5-3/M5-4 依此实现）。

---

## 1. 布局稿

整体为 ratatui 垂直三区（自上而下）：**转录流**（弹性）、**图像视口**（条件渲染）、**输入区 + 状态栏**（固定）。

```
┌──────────────────────────────────────────────────────┐
│ 转录流 (transcript)                            ↑↓滚动 │
│ ┌──────────────────────────────────────────────────┐ │
│ │ [step 1] browser.navigate          reused=false  │ │
│ │ thought: 打开新浪财经首页                          │ │
│ │ observation: ok url=https://finance.sina.com.cn   │ │
│ │ (screenshot → 图像视口 / 降级占位符)               │ │
│ ├──────────────────────────────────────────────────┤ │
│ │ [step 2] browser.click selector=#login reused=*  │ │
│ │ ...                                               │ │
│ └──────────────────────────────────────────────────┘ │
│ 图像视口 (viewport)：当前可见步的截图帧（≤终端高度 40%）│
├──────────────────────────────────────────────────────┤
│ 输入区 (input)：prompt…            Enter发送 Esc清空  │
├──────────────────────────────────────────────────────┤
│ 状态栏 (status)：steps=3 │ view=1/4 │ profile=stub  │
└──────────────────────────────────────────────────────┘
```

### 各区块与数据来源（SessionEvent 字段映射）

| 区块 | 内容 | 数据来源 |
|---|---|---|
| 转录流·步标题 | `[step N] {tool_name}` + `reused_script` 徽标 | `SessionEvent::Step.step.tool_name` / `step.reused_script` |
| 转录流·thought 行 | 模型思考/观察摘要 | `SessionEvent::Step.step.observation` |
| 转录流·observation 行 | 工具执行结果 | `SessionEvent::Step.step.observation`（合并结果 ok=execute.ok&&verify.ok） |
| 转录流/视口·图像 | 截图派发 | `SessionEvent::Step.step.image`（已 ingest 解码的临时文件路径） |
| 转录流·终态横幅 | 完成/空闲状态与消息 | `SessionEvent::Done.state.status` / `state.message` |
| 转录流·错误条 | 结构化错误（见 §4 失败态规范） | `SessionEvent::Error.message` |
| 输入区 | 用户 prompt 文本 | 本地输入状态（不来自事件） |
| 状态栏·步数 | 已完成步计数 | 已收到的 `SessionEvent::Step` 数量（本地累计） |
| 状态栏·view | 当前 view 池占用 | webai-tui `session.rs` 后台服务（`Arc<AgentSession>` 关联的 view 标识） |
| 状态栏·profile | 模型 profile 名 | `LlmClient::profile()`（经 session 后台透出） |

覆盖 §4.11 全部区块：`session.rs`（后台 mpsc 事件源）→ 转录流 + 状态栏；`app.rs`（ratatui 前端）→ 输入区 + 键位处理 + 图像视口派发。

---

## 2. 键盘映射表

与 ARCHITECTURE.md §4.11 逐项一致：

| 按键 | 行为 |
|---|---|
| `Enter` | 发送输入区内容为 prompt（空内容忽略） |
| `↑` / `↓` | 转录流逐行滚动 |
| `PgUp` / `PgDn` | 转录流整页翻页（页高 = 转录流区可视行数） |
| `Esc` | 清空输入区；输入区已为空时退出应用 |
| `Ctrl+C` | 立即退出（清理图像临时文件后恢复终端） |

补充约定（不与 §4.11 冲突）：
- 滚动跟随：新事件到达且用户位于底部时自动跟随；用户上滚后暂停跟随，按 `↓` 到底恢复。
- 输入区为多行安全：粘贴含换行文本时以 `␤` 显示，`Enter` 仍整条发送。

---

## 3. 图像渲染规范（FR-2 三元组之 screenshot）

### 3.1 ingest（解码一次 + 持久化）

- `SessionEvent::Step.step.image` 到达时为 base64 PNG 字符串（或已就绪的临时文件路径）。
- ingest 时**解码一次**，写入 `$TMPDIR/webai-tui-<session>/step-<n>.png`，路径登记进内存表；后续渲染只读文件，绝不重复解码。
- 退出（Esc 空输入 / Ctrl+C）时删除整个临时目录。

### 3.2 视口派发

- 仅对**进入终端视口的可见步**派发图像帧；滚出视口的步不占帧预算（防止长会话卡顿）。
- 图像视口高度 ≤ 终端高度的 40%；同一时刻只渲染最近一个可见步的图像。

### 3.3 终端能力协商与降级

按优先级探测：**Kitty → iTerm2 → Sixel → 占位符**。

| 协议 | 探测方式 | 帧格式 |
|---|---|---|
| Kitty graphics | `$TERM` 含 `kitty` 或响应 kitty 查询 | APC `G` 转义，直接传 PNG 文件 |
| iTerm2 | `$TERM_PROGRAM=iTerm.app` / `WezTerm` | OSC 1337 `File=...;base64` |
| Sixel | 终端响应 DA1 含 `;4;` | 六素图像（解码后的 PNG 转 sixel） |
| 均不支持 | — | **占位符**（见下） |

**占位符文案**（不支持图像的终端）：

```
📷 [screenshot unavailable]
reason: terminal does not support Kitty/iTerm2/Sixel image protocols
image saved: /tmp/webai-tui-<session>/step-3.png   ← 文件仍可查看
```

降级行为：
- 占位符占用与真实图像相同的布局槽位（保持排版稳定）；
- 临时文件**照常**持久化，用户可自行打开；
- 会话结束清理与正常路径一致。

---

## 4. 失败态视觉规范（FR-2 / 定论四）

- `SessionEvent::Error` 渲染为红色错误条，**必须**展示结构化内容：
  - `phase`（失败阶段：`compose` / `execute` / `verify` / `navigate` / `transport` …）
  - JS 异常**原文**（或底层错误原文）
- **禁止**出现"未知错误"（unknown error）字样：事件缺失结构化字段时，显示原始 `message` 全文；连 message 都缺失时显示 `phase=transport detail=empty error payload`（结构化兜底，而非"未知错误"）。
- 示例：

```
✗ phase=execute  detail=ReferenceError: x is not defined (step 3, browser.evaluate)
```

---

## 5. 旅程对照（PRODUCT-DESIGN.md §5）

| 旅程 | TUI 呈现 |
|---|---|
| A 首次使用 | 步进三元组逐条出现，`reused=false` |
| B 记忆复用 | 步标题带 `reused=true` 徽标，thought 说明命中记忆脚本 |
| C 失败恢复 | 错误条按 §4 规范展示 phase + 原文，会话可继续输入 |
| D 崩溃恢复 | 启动时经 recovery 重建转录流，历史步只读渲染，可继续写 |
