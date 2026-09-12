# webai-bridge-cxx

Type-safe Rust ↔ C++ bridge over the cog / WPEBackend-fdo C++ wrappers.

**This is the only crate in the workspace that may contain C++** (ARCHITECTURE.md
§4.7 / §10). All system-WebKit/C++ linking is gated behind the opt-in
`legacy_cpp` feature.

## Feature gating

- **default** (no `legacy_cpp`): pure Rust. Compiles and tests with **no**
  system WebKit, and every operation returns a structured `BridgeCxxError::CogLaunch`
  so upper layers can diagnose a missing FFI environment instead of crashing.
- **`legacy_cpp`**: pulls the `cxx` bridge to cog / libwpe. Only enabled on an
  actual WebKit build environment.

## Building with the real WPE stack

To build the real cog/WPE bridge, enable the `legacy_cpp` feature:

```sh
cargo build -p webai-bridge-cxx --features legacy_cpp
```

The `build.rs` probes the required system libraries via `pkg-config` and fails
with a **readable** error naming the missing package and an install hint (never
a link-time symbol soup).

### Required system libraries

| pkg-config package | version | install hint |
|---|---|---|
| `cogcore` | cog ≥ 0.19 | `apt install libcog-dev` |
| `wpe-webkit-2.0` | WPE WebKit ≥ 2.50 | `apt install libwpewebkit-2.0-dev` |
| `wpebackend-fdo-1.0` | WPEBackend-fdo ≥ 1.16 | `apt install libwpebackend-fdo-dev` |
| `cairo` | cairo | `apt install libcairo2-dev` |
| `glib-2.0` | GLib | `apt install libglib2.0-dev` |

### Runtime platform selection

The bridge launches a headless cog shell by default. Override the WPE backend
with the `COG_PLATFORM_NAME` environment variable (e.g. `headless`, `drm`,
`x11`).

## What the bridge does

The C++ wrapper is deliberately **thin**: it only does cog/WPE lifecycle and API
forwarding (view create/destroy, `load_uri`, `evaluate`, `screenshot`, and the
`WEBKIT_LOAD_FINISHED` callback). It implements **no browser command logic** —
all browser behaviour is expressed as JavaScript (定论一).

## Files

- `src/lib.rs` — `#[cxx::bridge]` definition + Rust facade (`WebkitBridgeCxx`)
- `cpp/wrapper.h` — C++ header (type aliases)
- `cpp/wrapper.cc` — thin cog/WPE wrapper
- `build.rs` — pkg-config probing + cxx-build glue
