# webai-ng — AI 浏览器

webai-ng（下称 **webai**）是一个"说句话就办成事"的 AI 浏览器：用户以自然语言下达任务（如"打开新浪财经并导出报表"），agent 循环自主 plan→act→observe，可 navigate / click / fill / evaluate / screenshot / download 等 13 个浏览器动词，并通过脚本记忆、崩溃恢复、资源护栏与结构化错误设计，面向可交付的稳定运行。

- **架构**：`docs/architecture/ARCHITECTURE.md`（分层 §3、错误处理 §7、构建 §10、测试金字塔 §9）
- **产品定义**：`docs/specs/PRODUCT-DESIGN.md`（§6 指标 M-1..M-7、§8 版本规划、FR-1..FR-8）
- **测试矩阵 / 发布门禁**：`docs/specs/e2e-test-matrix.md`、`docs/specs/release-gate-report-v0.4.md`、`docs/specs/release-review-v0.4.md`

---

## 目录结构

```
.
├── docs/
│   ├── architecture/ARCHITECTURE.md     架构说明
│   └── specs/                           PRODUCT-DESIGN / 测试矩阵 / 发布门禁
└── webai-ng/                            cargo workspace（仓库根下唯一 workspace）
    ├── bins/webai                       webai 可执行入口（TUI / --serve / --headless）
    ├── crates/                          分层 crate（protocol / config / script / agent /
    │                                    acp / tui / bridge / bridge-cxx / webkit / memory / llm / embedding）
    ├── ci/wpe.Dockerfile                legacy_cpp（真机桥）可复现镜像
    ├── scripts/                         check-dep-layers / rss_sample / rss_budget_crosscheck
    ├── fixtures/pages/                  基准页 fixture（static / spa）
    ├── page-bundle/                     浏览器 JS 脚本包（cog/WPE 注入用）
    ├── docs/wpe-image.md                WPE 镜像构建与升级流程
    └── docs/security-audit-69.md        安全审计记录
```

## 快速开始（默认 stub 路径，零系统 WebKit）

默认构建不依赖任何系统 WebKit：bridge/webkit 层在默认 feature 下返回结构化 `CogLaunch` 错误，方便在没有浏览器环境时开发 agent / ACP / TUI / memory 等纯 Rust 逻辑。

```bash
cd webai-ng
cargo build --workspace
cargo test --workspace
# 运行一个无 UI 的 headless 冒烟（stub executor，脚本驱动）
./target/debug/webai --headless --prompt "打开新浪财经" --config-dir /tmp/webai-smoke-cfg
```

## WPE 环境（legacy_cpp 真机桥）依赖与安装指南

`webai-bridge-cxx` 是 workspace 中**唯一允许包含 C++** 的 crate（架构 §4.7 / §10），负责与 cog / WPEBackend-fdo / WPE WebKit 的真机桥接。真机浏览器能力全部通过它的 `legacy_cpp` feature 打开；默认路径零系统 WebKit。

### 两条独立构建路径（§10 硬性要求）

| 路径 | feature | 依赖系统 WebKit | 用途 |
|---|---|---|---|
| 默认 stub | （无 `legacy_cpp`） | 否 | agent / ACP / TUI / memory 开发，可移植 |
| 真机桥 | `--features legacy_cpp` | 是（WPE 栈） | 真机 navigate/evaluate/snapshot 等浏览器动词 |

### 1. 运行时 + 开发库（Debian/Ubuntu）

`webai-bridge-cxx/build.rs` 通过 `pkg-config` 探测，缺失时给出**可读错误**并提示安装命令（不会出现 link-time 符号汤）。下表与 `webai-ng/ci/wpe.Dockerfile`、`crates/webai-bridge-cxx/README.md` 保持一致。

| pkg-config 包 | 建议版本 | 安装（Debian/Ubuntu） |
|---|---|---|
| `cogcore` | cog ≥ 0.19 | `apt install libcog-dev` |
| `wpe-webkit-2.0` | WPE WebKit ≥ 2.50 | `apt install libwpewebkit-2.0-dev` |
| `wpebackend-fdo-1.0` | WPEBackend-fdo ≥ 1.16 | `apt install libwpebackend-fdo-dev` |
| `cairo` | — | `apt install libcairo2-dev` |
| `glib-2.0` | GLib | `apt install libglib2.0-dev` |

Debian bookworm 发行版包（对应 `wpe-2.48-rust90` 镜像 tag）：

```bash
apt-get update && apt-get install -y --no-install-recommends \
  libwpe-1.0-1 libwpe-1.0-dev \
  libwpebackend-fdo-1.0-1 libwpebackend-fdo-1.0-dev \
  libwpewebkit-1.0-1 libwpewebkit-1.0-dev \
  cog \
  libglib2.0-dev libgtk-3-dev libcairo2-dev \
  libssl3 libssl-dev \
  build-essential pkg-config libclang-dev clang cmake ninja-build
```

> **不**建议现编 WPE WebKit（`cmake -DCMAKE_BUILD_TYPE=Release -DUSE_LIBBACKTRACE=OFF -DPORT=WPE -G Ninja`）——成本极高（架构 §10），仅当发行版包缺失时才走源编路线，并在 `webai-ng/docs/wpe-image.md` 版本记录表登记 cmake 参数。

### 2. 无头显示后端（headless）

cog 在无显示环境需要离屏渲染：

```bash
# 固定 WPEBackend-fdo 后端
export WPE_BACKEND=fdo
# 需要 X 显示的终端上，用 Xvfb 提供 virtual display
Xvfb :99 -screen 0 1024x768x24 &
export DISPLAY=:99
```

也可用 `COG_PLATFORM_NAME` 覆盖运行时平台（`headless` / `drm` / `x11`）。

### 3. Rust 工具链

```bash
cd webai-ng
# MSRV 1.87+，workspace 里 rust-version 已声明；实际用 stable
rustup toolchain install stable --profile minimal --component rustfmt,clippy
```

### 4. 构建并测试真机桥

```bash
cd webai-ng
cargo build   -p webai-bridge-cxx --features legacy_cpp
cargo test    -p webai-bridge-cxx --features legacy_cpp
# 真机冒烟（launch + preflight），带 --ignored 门
cargo test    -p webai-bridge-cxx --features legacy_cpp --test real_device_smoke -- --ignored --nocapture
```

默认 stub 路径仍应可用：

```bash
cargo build --workspace --no-default-features   # Rust-only，必须可移植（§4.7）
```

### 5. 一键可复现 WPE 环境（Docker 镜像）

无需手动装包时，使用仓库提供的可复现镜像（固定 tag，禁止 `latest`，见 `webai-ng/docs/wpe-image.md`）：

```bash
docker build -t orlfly/webai-ng-ci:wpe-2.48-rust90 -f webai-ng/ci/wpe.Dockerfile webai-ng/
docker run --rm -v "$PWD/webai-ng:/work" orlfly/webai-ng-ci:wpe-2.48-rust90 \
  bash -c "cargo build --features legacy_cpp -p webai-bridge-cxx"
```

---

## 分层与质量门禁

- **分层偏序**：`webai-ng/scripts/check-dep-layers.py --workspace .`（§3.2，CI `ci` job 首步，违例即失败）。
- **cargo-deny**：bans / sources / licenses（`webai-ng/deny.toml`）。
- **结构化错误**：全仓禁止裸 `"unknown error"`（M-4 零 unknown，见 PRODUCT-DESIGN §6）。
- **可复现构建**：同 commit 两次 release 构建需字节一致（`cmp`）。
- **M-1..M-7 门禁**：`docs/specs/release-gate-report-v0.4.md`。

## 贡献

- 开发完成后将开发分支经 **PR 合并回 `main`**（不直接 push 到 main），PR 需关联 Kaneo 任务号（`Closes #N`）。
- 提交前：`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all`。
- 真机相关改动需在 WPE 镜像内验证（见上）。
