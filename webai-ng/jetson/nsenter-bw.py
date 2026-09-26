#!/usr/bin/env python3
"""Enter Debian bookworm rootfs as root via unprivileged namespaces.

Field-tested shape (probe5) on this Jetson 5.10 tegra kernel:
  - unshare(USER|NS|PID) runs in the ORIGINAL process, then os.fork();
    that fork is the first process (pid1) of the new pidns and only in/after
    it does mount(proc) succeed (mounting from the original process errors
    EPERM on this kernel).
  - The single-entry uid/gid map is written by the parent via
    /proc/<child-pid>/... right after fork (before doing anything that needs
    mapped identity; mount/chroot do it earlier race-free).

Usage: python3 nsenter-bw.py '<shell commands>'
"""
import ctypes
import os
import sys

ROOTFS = os.environ.get("BW_ROOTFS", "/home/nv/.jcode/scratch/bookworm/rootfs")
UID, GID = os.getuid(), os.getgid()

libc = ctypes.CDLL("libc.so.6", use_errno=True)
MS_BIND, MS_REC = 4096, 16384

os.makedirs(ROOTFS + "/proc", exist_ok=True)

libc.unshare(0x10000000 | 0x20000000 | 0x00020000)  # USER|NS|PID
pid = os.fork()
if pid == 0:
    R = ROOTFS.encode()
    rc = libc.mount(b"proc", R + b"/proc", b"proc", 0, None)
    rcd = libc.mount(b"/dev", R + b"/dev", None, MS_BIND | MS_REC)
    rcr = libc.mount(b"/run", R + b"/run", None, MS_BIND | MS_REC)
    rch = libc.mount(b"/home", R + b"/home", None, MS_BIND | MS_REC)
    rcs = libc.mount(b"/sys", R + b"/sys", None, MS_BIND | MS_REC)
    rct = libc.mount(b"tmpfs", R + b"/tmp", b"tmpfs", 0, None)
    libc.chroot(R)
    os.chdir("/")
    os.execve("/bin/bash", ["/bin/bash", "-c", " ".join(sys.argv[1:])],
              dict(PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                   HOME="/root", DEBIAN_FRONTEND="noninteractive",
                   TERM=os.environ.get("TERM", "xterm")))
try:
    sg = os.open("/proc/%d/setgroups" % pid, os.O_WRONLY)
    os.write(sg, b"deny\n")
    os.close(sg)
    g = os.open("/proc/%d/gid_map" % pid, os.O_WRONLY)
    os.write(g, ("0 %d 1\n" % GID).encode())
    os.close(g)
    u = os.open("/proc/%d/uid_map" % pid, os.O_WRONLY)
    os.write(u, ("0 %d 1\n" % UID).encode())
    os.close(u)
except OSError as e:
    sys.stderr.write("map write failed: %s\n" % e)
_, status = os.waitpid(pid, 0)
if os.WIFEXITED(status):
    sys.exit(os.WEXITSTATUS(status))
sys.stderr.write("child killed by signal %d\n" % os.WTERMSIG(status))
sys.exit(128 + os.WTERMSIG(status))