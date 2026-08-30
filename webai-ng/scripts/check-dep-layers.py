#!/usr/bin/env python3
"""Layering enforcement for the webai-ng cargo workspace.

Parses `cargo metadata` and asserts the workspace-crate dependency partial
order described in ARCHITECTURE.md §3.2 "分层依赖规则（强制）":

    protocol < config < {llm, embedding, script}
    embedding < memory
    {script, webkit} < bridge                 (script || webkit, no edge between them)
    {llm, memory, bridge} < agent
    agent < {acp, tui} < bins/webai           (acp || tui, no edge between them)

Extra invariants asserted here:
  * webai-protocol depends on zero other workspace crates (serde only).
  * The only workspace crate that may link C++ (declare a `cxx` dependency or a
    `legacy_cpp` feature) is webai-bridge-cxx.

Exit code 0 on success, 1 on any violation (fails the CI build).

Usage:
    check-dep-layers.py [--workspace DIR]
"""

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

# Numeric layer per workspace crate. An edge (dep -> pkg) is only legal when
# layer[dep] < layer[pkg] and the pair is not listed in FORBIDDEN_PAIRS.
LAYERS = {
    "webai-protocol": 0,
    "webai-config": 1,
    "webai-llm": 2,
    "webai-embedding": 2,
    "webai-script": 2,
    "webai-memory": 3,
    "webai-webkit": 3,
    "webai-bridge": 4,
    "webai-bridge-cxx": 2,
    "webai-agent": 5,
    "webai-acp": 6,
    "webai-tui": 6,
    "webai": 7,  # bins/webai — assembly binary
}

# Crates explicitly declared parallel/mutually independent in §3.2. Any direct
# dependency edge between these pairs is a violation.
FORBIDDEN_PAIRS = {
    frozenset(("webai-llm", "webai-embedding")),
    frozenset(("webai-llm", "webai-script")),
    frozenset(("webai-embedding", "webai-script")),
    frozenset(("webai-script", "webai-webkit")),
    frozenset(("webai-acp", "webai-tui")),
}


def validate(packages: dict) -> list:
    """Return a list of human-readable violation messages for the given cargo
    metadata `packages` mapping (name -> package dict). Empty list == OK."""
    unknown = set(LAYERS) - set(packages)
    if unknown:
        return ["workspace crates missing from manifest: %s" % ", ".join(sorted(unknown))]

    # Direct workspace-crate dependency edges: (dep_name, pkg_name).
    edges = []
    for pkg in packages.values():
        if pkg["name"] not in LAYERS:
            continue
        for dep in pkg["dependencies"]:
            # kind == null  -> normal/runtime dependency; skip dev- & build-deps
            if dep["kind"] is not None:
                continue
            if dep["name"] in LAYERS:
                edges.append((dep["name"], pkg["name"]))

    errors = []

    # 1) webai-protocol must have zero workspace-crate dependencies.
    protocol_deps = [d for (d, _) in edges if _ == "webai-protocol"]
    if protocol_deps:
        errors.append(
            "webai-protocol must have zero workspace-crate dependencies "
            "(ARCHITECTURE.md §3.2): depends on %s" % ", ".join(sorted(protocol_deps))
        )

    # 2) Partial-order / parallel-pair enforcement.
    for dep, pkg in sorted(edges):
        pair = frozenset((dep, pkg))
        if pair in FORBIDDEN_PAIRS:
            errors.append(
                "forbidden edge: `%s` dep `%s` — §3.2 declares them parallel, "
                "they must not depend on each other" % (pkg, dep)
            )
            continue
        if LAYERS[dep] >= LAYERS[pkg]:
            errors.append(
                "layering violation: `%s` (layer %d) dep `%s` (layer %d); "
                "dependencies may only flow left-to-right in the §3.2 partial order"
                % (pkg, LAYERS[pkg], dep, LAYERS[dep])
            )

    # 3) Only webai-bridge-cxx may contain/link C++ (`cxx` dep or `legacy_cpp`).
    for pkg in packages.values():
        if pkg["name"] not in LAYERS:
            continue
        dep_cxx = any(d["name"] in ("cxx", "cxxbridge") for d in pkg["dependencies"])
        feature_legacy = "legacy_cpp" in pkg.get("features", {})
        if (dep_cxx or feature_legacy) and pkg["name"] != "webai-bridge-cxx":
            errors.append(
                "C++ isolation violated: `%s` declares `%s` a C++-related dep and/or "
                "`legacy_cpp` feature; only webai-bridge-cxx may (ARCHITECTURE.md §4.7)"
                % (pkg["name"], "cxx" if dep_cxx else "legacy_cpp")
            )

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--workspace",
        default=".",
        help="Path to the workspace root (default: current directory).",
    )
    args = parser.parse_args()

    workspace = Path(args.workspace).resolve()
    if shutil.which("cargo") is None:
        print("error: `cargo` not found in PATH")
        return 1

    try:
        # Read the full resolved metadata. This covers all declared dependencies
        # regardless of which feature flags are active, so the layer edges are
        # verified against the crate structure itself (the `--no-default-features`
        # build is enforced as a separate CI step, not here).
        proc = subprocess.run(
            ["cargo", "metadata", "--format-version", "1"],
            cwd=workspace,
            check=True,
            capture_output=True,
            text=True,
        )
    except subprocess.CalledProcessError as exc:
        print("error: `cargo metadata` failed")
        print(exc.stderr)
        return 1

    meta = json.loads(proc.stdout)
    packages = {p["name"]: p for p in meta["packages"]}

    errors = validate(packages)

    if not errors:
        print(
            "✓ layering OK: %d workspace crates — all dependency edges respect "
            "ARCHITECTURE.md §3.2 partial order" % len(packages)
        )
        return 0

    print("LAYERING VIOLATIONS FOUND (%d):" % len(errors))
    for line in errors:
        print("  - " + line)
    return 1


if __name__ == "__main__":
    sys.exit(main())
