# webai-ng release notes

## 0.1.0 (M6 packaging baseline, task 84 / Kaneo #67)

### Binary size

Measured on a clean `cargo build --release -p webai` with the workspace
`[profile.release]` configuration:

| item | value |
| --- | --- |
| binary | `target/release/webai` |
| size | 350192 bytes (~342 KiB) |
| embedded bundle | 15 modules, 38903 bytes |
| self-check | `webai ... bundle self-check ok: modules=15, bytes=38903` |

### Release profile

```toml
[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
```

### Bundle single fact source

The embedded page-bundle table (`webai_webkit::bundle::BUNDLE_SOURCES`) is the
only copy of each module in the binary; `bundle_script()` reads the table by
path. The duplicate stub copy under `crates/webai-webkit/page-bundle/` was
removed; the workspace-root `page-bundle/` assets (task 30 migration) are the
canonical source.

### Startup self-check

`bins/webai` runs `self_check()` before any UI mode. Failure prints a
structured error to stderr and exits with status 2.
