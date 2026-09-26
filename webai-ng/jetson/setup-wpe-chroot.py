#!/usr/bin/env python3
"""Set up the rootless WPE build chroot for webai-ng on this Jetson host.

WHAT IT DOES (idempotent, safe to re-run):
 1. Downloads the Debian trixie generic-arm64 cloud image (.raw) from the
    NJU mirror if not already present.
 2. Extracts the rootfs partition via dd + debugfs rdump (fully unprivileged,
    no docker, no fuse, no root).
 3. Configures apt (USTC mirror, sandbox disabled) and installs the WPE
    toolchain deps: WPE WebKit 2.48 dev, libwpe, WPEBackend-fdo, cairo, glib,
    meson/ninja, pkg-config, g++, git.
 4. Builds & installs cog 0.19.1 from source (Debian ships 0.18.4, which
    lacks cog_init/cog_view_new used by webai-bridge-cxx).
 5. Compiles the extern-C snapshot shim (libWPECompat.a) that provides the
    WebKitGTK-style webkit_web_view_get_snapshot / webkit_image_* symbols,
    which do not exist in any WPE WebKit build, and wires it into the
    wpe-webkit-2.0.pc Libs line (-lWPECompat).
 6. Reuses the rustup toolchain from the bookworm chroot (static ELF).

USAGE:
  enter:  BW_ROOTFS=/home/nv/.jcode/scratch/trixie/rootfs \
            python3 /home/nv/.jcode/scratch/nsenter-bw.py '<cmd>'
  build:  (inside) source /root/.cargo/env
            cd /home/nv/webai/webai-ng
            export CARGO_TARGET_DIR=/home/nv/.jcode/scratch/trixie-target
            export RUSTFLAGS="-C link-arg=-Wl,--allow-shlib-undefined"
            cargo test -p webai-bridge-cxx --features legacy_cpp \
              --test real_device_smoke -- --ignored

WHY THESE CHOICES (field-tested on Jetson kernel 5.10.192-tegra):
  - Docker unusable: daemon runs but user is not in the docker group; sudo
    needs a password. Rootless userns chroot is the only path.
  - Kernel quirks: full-range uid maps and setgroups=allow fail EPERM; only
    single-entry map (0 -> uid 1000) works. Mounting procfs only succeeds
    when unshare(USER|NS|PID) ran in an ancestor process and the mount
    happens after a fork (probe5 shape); mounting from the same process
    errors EPERM on this tegra kernel. bash fork ENOMEM earlier in this
    session was caused by running os.system (fork) as the new pidns' pid1
    before /proc existed in the new mount ns.
  - Debian bookworm (glibc 2.36) cannot run WPE WebKit 2.48 (needs 2.38)
    and Debian's bookworm WPE is 2.38.6 which lacks the evaluate_javascript
    API wrapper uses. trixie has both glibc 2.41 and WPE 2.48.
  - The snapshot API simply does not exist in WPE WebKit (any version), it
    is a WebKitGTK-only API; the shim returns null so callers get a
    structured error instead of a link error.
"""
import ctypes
import os
import subprocess
import sys

SCRATCH = "/home/nv/.jcode/scratch"
TRIXIE_DIR = SCRATCH + "/trixie"
ROOTFS = TRIXIE_DIR + "/rootfs"
RAW_URL = ("https://mirror.nju.edu.cn/debian-cdimage/cloud/trixie/latest/"
           "debian-13-generic-arm64.raw")
RAW = SCRATCH + "/trixie.raw"

APT_SOURCES = [
    "deb https://mirrors.ustc.edu.cn/debian trixie main",
    "deb https://mirrors.ustc.edu.cn/debian-security trixie-security main",
]

DEPS = [
    "pkg-config", "libglib2.0-dev", "libcairo2-dev",
    "libwpe-1.0-dev", "libwpebackend-fdo-1.0-dev", "libwpewebkit-2.0-dev",
    "g++", "ninja-build", "meson", "python3", "curl", "ca-certificates",
    "xz-utils", "gcc", "git", "libasound2",
]


def sh_inside(cmd, timeout=3600):
    env = dict(os.environ, BW_ROOTFS=ROOTFS)
    p = subprocess.run(["python3", SCRATCH + "/nsenter-bw.py", "bash -c", cmd],
                       capture_output=True, text=True, timeout=timeout, env=env)
    if p.returncode != 0:
        sys.stderr.write("chroot command failed: %s\n%s\n" % (cmd, p.stderr))
    return p


def download(url, dest):
    print("downloading %s" % url)
    subprocess.run(["curl", "-sSf", "--max-time", "3600", "-C", "-", url, "-o", dest],
                   check=True)


def main():
    os.makedirs(TRIXIE_DIR, exist_ok=True)
    if not os.path.exists(RAW):
        download(RAW_URL, RAW)
    if not os.path.exists(ROOTFS + "/usr/bin/bash"):
        # partition table: root at sector 262144, 6027264 sectors (trixie cloud)
        subprocess.run(["dd", "if=" + RAW, "of=" + TRIXIE_DIR + "/root.img",
                        "bs=512", "skip=262144", "count=6027264", "status=none"],
                       check=True)
        os.makedirs(ROOTFS, exist_ok=True)
        subprocess.run(["debugfs", "-R", "rdump / " + ROOTFS,
                        TRIXIE_DIR + "/root.img"], capture_output=True, check=True)
        print("rootfs extracted")
    sh_inside(
        "printf '%s\\n' %s > /etc/apt/sources.list && "
        "echo 'APT::Sandbox::User=\"\";' > /etc/apt/apt.conf.d/99sandbox && "
        "export DEBIAN_FRONTEND=noninteractive && "
        "apt-get -o APT::Sandbox::User= update -q"
        % tuple(APT_SOURCES))
    sh_inside("export DEBIAN_FRONTEND=noninteractive && apt-get "
              "-o APT::Sandbox::User= install -y --no-install-recommends "
              + " ".join(DEPS), timeout=1800)
    # cog tarball: prefer local cache, else fetch from GitHub codeload
    # (codeload works on this network even when github.com web UI is flaky).
    COG_TAR = "/home/nv/.jcode/scratch/cog-0.19.1.tar.gz"
    if not os.path.exists(COG_TAR):
        subprocess.run([
            "bash", "-c", "for i in 1 2 3 4 5; do "
            "curl -sSf --max-time 300 -C - -o '" + COG_TAR + "' "
            "https://codeload.github.com/Igalia/cog/tar.gz/refs/tags/v0.19.1 "
            "&& exit 0; sleep 5; done; exit 1"
        ], check=True)
    sh_inside(
        "mkdir -p /usr/local/src && cd /usr/local/src && "
        "tar xf " + COG_TAR + " && "
        "cd cog-0.19.1 && meson setup build --prefix=/usr/local "
        "-Dplatforms=headless -Dwpe_api=2.0 && ninja -C build && "
        "ninja -C build install && ldconfig", timeout=1800)
    sh_inside(
        "cp /home/nv/webai/webai-ng/jetson/wpe_compat.cpp /var/tmp/ && "
        "cd /var/tmp && g++ -std=c++20 -fPIC -c wpe_compat.cpp "
        "-o wpe_compat_t.o -I/usr/include/glib-2.0 "
        "-I/usr/lib/aarch64-linux-gnu/glib-2.0/include && "
        "ar rcs /usr/local/lib/libWPECompat.a wpe_compat_t.o && "
        r'''python3 -c "
p='/usr/lib/aarch64-linux-gnu/pkgconfig/wpe-webkit-2.0.pc'
s=open(p).read()
if '-lWPECompat' not in s:
    s=s.replace('-lWPEWebKit-2.0','-lWPEWebKit-2.0 -lWPECompat')
    open(p,'w').write(s)
print('pc patched')
"''')
    sh_inside("/root/.cargo/bin/cargo --version", timeout=120)
    print("ALL DONE — pkg-config triple:")
    print(sh_inside("pkg-config --modversion cogcore wpe-webkit-2.0 "
                    "wpebackend-fdo-1.0").stdout)


if __name__ == "__main__":
    main()