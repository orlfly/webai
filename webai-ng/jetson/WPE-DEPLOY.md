# webai-ng WPE 栈部署（Jetson Xavier NX，无 docker/无 root）

## 成果

- `cargo build -p webai-bridge-cxx --features legacy_cpp` 成功
- 冒烟测试 `real_device_launch_and_preflight` **通过**（真实启动 WPE WebKit + cog 后端）

## 环境

| 组件 | 版本 | 来源 |
|---|---|---|
| glibc | 2.41 | Debian trixie rootfs（chroot）|
| WPE WebKit | 2.48.3（wpe-webkit-2.0.pc）| trixie apt（USTC 镜像）|
| libwpe | 1.16.2 | trixie apt |
| WPEBackend-fdo | 1.16.0 | trixie apt |
| cog | **0.19.1**（源码构建）| codeload.github.com tarball |
| Rust | 1.90.0 | rustup（从 bookworm chroot 复制）|
| shim | libWPECompat.a（6 符号，extern C）| 本地源码 |

路径：rootfs 由 `setup-wpe-chroot.py` 重建，默认落在 `/home/nv/.jcode/scratch/trixie/rootfs`
（同名 bookworm rootfs 已弃用：glibc 2.36 跑不了 WPE 2.48）。
本目录所有脚本/文档均为自宿主直接可跑；chroot 内无需 docker 权限。

## 快速用法

```bash
# 进入 chroot（BW_ROOTFS 指向 trixie；不带变量时默认 bookworm，需显式指定）
BW_ROOTFS=/home/nv/.jcode/scratch/trixie/rootfs \
  python3 jetson/nsenter-bw.py '<命令>'

# 一键重建整套环境（幂等）
python3 jetson/setup-wpe-chroot.py
```

构建 + 冒烟（chroot 内）：

```bash
source /root/.cargo/env
cd /home/nv/webai/webai-ng
export CARGO_TARGET_DIR=/home/nv/.jcode/scratch/trixie-target
export RUSTFLAGS="-C link-arg=-Wl,--allow-shlib-undefined"
cargo test -p webai-bridge-cxx --features legacy_cpp \
  --test real_device_smoke -- --ignored
# 期望输出：test real_device_launch_and_preflight ... ok；TEST_EXIT=0
```

## 关键决策与根因

1. **docker 不可用**：daemon 在跑但 nv 不在 docker 组；sudo 需密码。方案 = 无特权的
   `unshare(USER|NS|PID)` + chroot。
2. **tegra 5.10 内核 namespace 行为**（ probes 实测）：
   - uid_map 只允许单条 `0 1000 1`；`setgroups=deny` 先写。
   - **挂 proc 只有在「unshare 后 fork 出的第一个进程」内才成功**；同进程内挂报 EPERM。
   - pid1 里先 `os.system`（fork bash）会 ENOMEM —— 因 pidns 第一个进程内 fork 在
     /proc 未挂时被拒；先挂 proc 再进 bash。
3. **glibc 墙**：WPE 2.48 要求 GLIBC_2.38；bookworm 2.36 停在 2.38.6 的 WPE（缺
   `evaluate_javascript` 系列 API）。chroot 直接用 trixie（glibc 2.41）一步到位，
   不再走 trixie 包混合注入。
4. **cog 必须 0.19+**：Debian 全系最高 0.18.x，缺 `cog_init/cog_platform_get/cog_view_new`。
   用 meson `-Dplatforms=headless -Dwpe_api=2.0` 源码构建。
5. **snapshot API 在 WPE WebKit 根本不存在**（WebKitGTK-only）：`webkit_web_view_get_snapshot`
   与 `webkit_image_*` 任何版本 WPE 库都没有。wrapper.cc 引用了它们 → 提供
   `libWPECompat.a`（返回 null 的 extern C stub）并注入 `.pc` 的 `Libs:` 行
   （`-lWPEWebKit-2.0 -lWPECompat`）。screenshot verb 会得到结构化错误而非崩溃；
   如需真实截图需要后续用 readback/其他 API 实现 shim。
6. **链接期 `--allow-shlib-undefined`**：WPE 2.48 的传递依赖（icu76/flite/jxl/avif…）
   不在链接集内时，用 `RUSTFLAGS="-C link-arg=-Wl,--allow-shlib-undefined"` 跳过
   shlib 传递符号检查（运行时由 ld.so 惰性解析，trixie 环境下全都能解析）。
7. **cargo metadata**：build.rs 里 `probe()` 用 pkg-config，`cargo_metadata(true)`
   自动把 pc 的 Libs 传给 rustc，`-lWPECompat` 注入即生效，无需改项目文件。

## 已知限制

- `snapshot/shim` 是 stub：screenshot 返回错误通道（不 crash）。真实截图需补
  readback 实现（e.g. 用 WPE 的 EGL readback 或 webkit_web_view_get_title 等
  替代输出）。
- webai-bridge 的 real_device_e2e/matrix 大量用 screenshot/paint 断言，在当前
  shim 下只有 launch/evaluate/navigate/标题类 verb 可全绿（evaluate 与 navigate
  完整可用，本次冒烟已验证 launch+preflight）。
- 每次 nsenter 均新建 namespace；长驻服务不受影响，但不共享跨会话挂载。

## 文件清单

| 文件 | 用途 |
|---|---|
| `jetson/nsenter-bw.py` | 通用 nsenter chroot 工具（`BW_ROOTFS` 可切 rootfs）|
| `jetson/setup-wpe-chroot.py` | trixie 环境一键重建 |
| `jetson/wpe_compat.cpp` | snapshot shim 源码 |
| `scratch/trixie/rootfs` | trixie rootfs（788M+deps，由脚本重建）|
| `scratch/cog-0.19.1.tar.gz` | cog 源码缓存（脚本自动从 codeload 拉取亦可）|
| `scratch/smoke-trixie.log` | 冒烟通过证据（TEST_EXIT=0）|