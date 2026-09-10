# Error Code Catalog

This document catalogs every structured error code used across the webai-ng
bridge and protocol layers. It corresponds 1:1 with the `codes` module in
`crates/webai-protocol/src/lib.rs` (ARCHITECTURE.md §4.1 / §7).

Every failure must carry a structured `error.code` (never a bare string, never
`unknown error`), a human-readable `message`, and where applicable a `phase`
(`execute` / `verify`) plus the JS exception text in `detail` (定论二).

## JSON-RPC standard codes

| constant | value | meaning |
|---|---|---|
| `PARSE_ERROR` | -32700 | Malformed JSON in an incoming message |
| `INVALID_REQUEST` | -32600 | Request is not a well-formed protocol object |
| `METHOD_NOT_FOUND` | -32601 | Method name is not registered |
| `INVALID_PARAMS` | -32602 | Params do not match the method's signature |
| `INTERNAL_ERROR` | -32603 | Unrecoverable server error |

## Domain-specific codes (reserved range -32099..-32000)

| constant | value | meaning |
|---|---|---|
| `LOAD_TIMEOUT` | -32001 | `bridge.wait_for_load` timed out before `WEBKIT_LOAD_FINISHED` |
| `PATH_NOT_ALLOWED` | -32002 | Path rejected by the filesystem-tool allowlist |
| `EXECUTE_FAILED` | -32003 | A browser-tool execute phase failed (JS exception) |
| `VERIFY_FAILED` | -32004 | A browser-tool verify phase failed (post-condition not met) |
| `MISSING_ARG` | -32005 | A browser-tool request was missing a required argument |

## Guarantees (M-4)

- **No `unknown error`**: every failure path surfaces a structured code + reason.
- **Phase + JS text**: script failures carry `phase ∈ {execute, verify}` and the
  JS exception text in `detail`.
- **Non-zero codes**: every code constant is non-zero and falls in its reserved
  range (enforced by unit tests).
- **Serialization round-trip**: `BrowserToolError` preserves `code`, `phase`,
  and `detail` across JSON serialization (enforced by unit tests).
