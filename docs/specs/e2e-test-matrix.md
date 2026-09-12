# 端到端用例矩阵（13 动词 × 环境变体）与 spec-coverage 审计

> 任务：M-1 v0.4 验收门的用例矩阵（PRODUCT-DESIGN.md §6 M-1 / M-7）。
> 基准页：`webai-ng/fixtures/pages/static.html` / `spa.html`（禁止现场自选页面）。
> 环境：stub（`cargo test`）与真机（WPE 镜像，CI `legacy_cpp` job）。

## 1. 环境变体定义

| 变体 | 说明 | 基准载体 |
|---|---|---|
| V1 static | 静态多要素页，无 JS | `fixtures/pages/static.html` |
| V2 spa | XHR 渲染（JS 重渲染等价物），等 `spa-ready` | `fixtures/pages/spa.html` |
| V3 authed | 登录态（先 navigate→fill→click 登录，再执行用例） | static（本地表单段） |
| V4 captcha-front | 验证码前置：用例预期停在拦截并产出结构化错误 | static（拦截段） |

## 2. 用例矩阵（13 动词）

每行：`用例 ID | 动词 | 变体 | 输入指令 | 预期产物 | 校验断言`。
"通过"定义 = execute.ok && verify.ok（FR-2 双阶段）且断言全部成立。

| ID | 动词 | 变体 | 输入 | 预期产物 | 断言 |
|---|---|---|---|---|---|
| N-S1 | navigate | V1 | `navigate url=<static>` | BrowserToolResponse ok | `location.href` 以 static.html 结尾 |
| N-S2 | navigate | V3 | 登录态下 navigate | ok | href 保持 + 会话 cookie 视图复用 |
| N-C1 | click | V1 | `click selector=#rows a:first` | ok | href 跳转生效（hash 变化） |
| N-C2 | click | V4 | `click selector=#captcha-btn` | 结构化错误（拦截） | error.code != 空，无 unknown |
| N-F1 | fill | V1 | `fill #q value=hello` | ok | input.value == hello |
| N-F2 | fill | V3 | 登录表单 fill 用户名/密码 | ok | 两输入框值正确 |
| N-H1 | hover | V1 | `hover selector=.row h2` | ok | :hover 样式触发（getComputedStyle） |
| N-D1 | drag | V1 | `drag #a → #b` | ok | 拖放标志位被置位 |
| N-P1 | pressKey | V1 | `pressKey key=Enter` | ok | keydown 事件捕获 |
| N-E1 | evaluate | V2 | `evaluate script=document.title` | "spa-ready" | 返回值匹配 |
| N-E2 | evaluate | V1 | 1MB 脚本 payload | ok 或结构化失败 | 永不 panic，无 unknown |
| N-SC1 | screenshot | V1 | 截图 | base64 PNG | PNG 签名 + 尺寸>0 |
| N-SC2 | screenshot | V2 | SPA 渲染后截图 | base64 PNG | 内容非空白（哈希与 V1 不同） |
| N-AT1 | accessibilityTree | V1 | 读树 | 树 JSON | 节点数 ≥ fixture sections |
| N-GT1 | getText | V1 | 读全文 | 文本 | 含 "Static benchmark page" |
| N-GT2 | getText | V2 | 渲染后读 | 文本 | 含 "item 0" |
| N-GH1 | getHtml | V1 | 读 body HTML | HTML | 含 `<section` ≥20 次 |
| N-DL1 | download | V1 | `download url=… filename=f.bin` | 落盘文件 | 文件存在且大小>0，路径在 allowlist 内 |
| N-DL2 | download | V4 | `../escape` 路径 | 结构化拒绝 | PATH_NOT_ALLOWED，0 穿越 |
| N-SN1 | snapshot | V1 | 一次性快照 | {href,title,readyState,text} | readyState==complete |
| N-SN2 | snapshot | V2 | 渲染后快照 | 同上 | text 含 spa 渲染项 |

计 21 个主用例；每用例失败时登记 `ID + 失败阶段(execute|verify|transport) + 原因原文`。

## 3. 通过率口径（M-1 ≥95%）

通过率 = 通过用例数 / (总用例数 − 非用例自身原因的环境失败)。
环境失败（镜像无显示、网络断）单独登记并重跑一次；重跑仍失败按真实失败计。
stub 层全部用例可在 `cargo test` 内以确定性 fake 驱动（已在 bridge/acp 契约测试覆盖执行与校验路径）；真机层由 CI `legacy_cpp` job 执行并输出通过率。

## 4. spec-coverage 审计（FR / 指标 ↔ 用例）

| Spec 项 | 覆盖用例 |
|---|---|
| FR-1 create_plan-first | m6_matrix release gates（plan 注入断言）+ N-E1 多步链 |
| FR-2 三元组呈现 + 双阶段 ok | 全部用例（execute+verify）；N-SC1/SC2 image 通道 |
| FR-3 记忆复用 | journey A/B（reused_script 真实 recall） |
| FR-4 下载 allowlist | N-DL1/N-DL2 |
| FR-5 崩溃恢复 | journey D + session_log 5 用例（M-3 100%） |
| FR-8 资源/安全护栏 | N-DL2（穿越 0 例）、pairing 门测试、max_steps 守卫测试 |
| M-1 ≥95% | 本矩阵（§3 口径） |
| M-4 零 unknown | journey C 断言 + 全 crate thiserror 审计 |
| M-5 穿越/覆盖 0 例 | N-DL2 + validate_session_id 单测 |

可追溯性：每个 FR/指标至少一个用例或一个具名测试；无"孤儿需求"。

## 5. 登记清单（初始为空）

| 用例 ID | 失败阶段 | 原因（原文） | 状态 |
|---|---|---|---|
| （待真机 CI 首跑填写） | | | |

stub 层当前：0 未通过。
