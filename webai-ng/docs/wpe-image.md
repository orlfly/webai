# WPE CI 镜像（legacy_cpp 路径）

> 任务：CI/WPE 容器镜像。目标：真机桥（`legacy_cpp`）在可复现环境中构建与测试，§10 的"默认构建零系统 WebKit"由两条独立 CI 路径保证。

## 1. 镜像

- Dockerfile：`webai-ng/ci/wpe.Dockerfile`
- 基础：`debian:bookworm-slim`，WPE 栈用发行版包（`libwpewebkit-1.0-dev` 等）+ `cog`，**不**现编 WPE WebKit（§10：cmake 源编成本极高，仅发行版包不可用时才走 `-DPORT=WPE -G Ninja` 源编路线）。
- Rust：1.90.0（满足 workspace rust-version 1.87+）。

## 2. 本地构建

```bash
docker build -t orlfly/webai-ng-ci:wpe-2.50-rust90 -f webai-ng/ci/wpe.Dockerfile webai-ng/
```

镜像内验证：

```bash
docker run --rm -v "$PWD/webai-ng:/work" orlfly/webai-ng-ci:wpe-2.50-rust90 \
  bash -c "cargo build --features legacy_cpp -p webai-bridge-cxx"
```

## 3. Tag 约定

`orlfly/webai-ng-ci:wpe-<WPEWEBKIT主.次>-rust<Rust次版本>`，如 `wpe-2.50-rust90`。

**CI 必须引用固定 tag，禁止 `latest`**（`ci.yml` 中 `container.image` 写死）。

## 4. WPE 版本升级流程

1. 查询 Debian bookworm(-backports) 的 `libwpewebkit-1.0` 新版本；
2. 改 Dockerfile（如需 backports 源则加一行 apt source，分层不变）；
3. 本地重建并打新 tag `wpe-<新版本>-rust<xx>`；
4. 更新 `ci.yml` 的 image tag；本地跑一次 legacy_cpp smoke（`cargo test -p webai-bridge-cxx --features legacy_cpp --test legacy_cpp_smoke -- --ignored`）；
5. 在本文件"版本记录"表追加一行。

### 版本记录

| 镜像 tag | WPE WebKit | libwpe | cog | Rust | cmake 参数（如源编） |
|---|---|---|---|---|---|
| wpe-2.50-rust90 | 2.48.x（bookworm 包） | 1.16.x | 0.19.x | 1.90.0 | 未源编（发行版包） |

## 5. 缓存策略与冷构建时长

| 层 | 缓存方式 | 说明 |
|---|---|---|
| apt 层 | Docker 分层缓存 | 依赖列表不变则不重装（约 3–5 分钟） |
| rustup 层 | Docker 分层缓存 | 固定工具链版本，命中则秒过（约 1–2 分钟） |
| cargo registry | GitHub Actions `Swatinem/rust-cache@v2`（`workspaces: webai-ng`） | registry + target 缓存 |
| target 目录 | 同上 | legacy_cpp feature 单独 key |

**冷构建实测**（首次无缓存）：apt ≈ 4 min + rustup ≈ 2 min ≈ **6 分钟**；CI 内 cargo 增量由 rust-cache 承担。docker layer 推送后热构建 < 1 分钟。

## 6. 与默认 stub 构建的关系

- CI 两条**独立**路径（§10 硬性要求）：`ci` job（默认 feature，stub，无任何系统 WebKit 依赖）与 `legacy_cpp` job（本镜像，WPE 真机桥），互不阻塞。
- 镜像内运行 headless/off-screen：`WPE_BACKEND=fdo` + xvfb（smoke 测试自带 `--ignored` 门），无显示环境时 WPEBackend-fdo 走 headless 合成。
