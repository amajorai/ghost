# Ghost

> A desktop-automation MCP server: screen perception and input control. Part of [Ryu](../../README.md).

[![License](https://shieldcn.dev/badge/License-Apache--2.0-73DC8C.svg?logo=apache&logoColor=white)](./LICENSE)
[![Stack](https://shieldcn.dev/badge/Rust-MCP-dea584.svg?logo=rust&logoColor=white)](../../README.md)

Ghost is a Rust MCP server that gives an agent eyes and hands on the desktop — 31 tools for screen capture/OCR, UI element detection, accessibility actions, mouse/keyboard control, and token-efficient element refs (`ghost_snapshot` captures a skeleton once; `ghost_click`/`ghost_type` act on a ref). It is cross-platform (Windows, macOS, Linux) and speaks the Model Context Protocol over stdio. Within Ryu it is callable via Core's MCP registry (`POST /api/mcp/tools/call`) and is not surfaced in the desktop UI.

**Tier:** OSS, self-hostable — Apache-2.0

## Stack

- Rust, MCP over stdio (no port)
- `ghost-core`, `ghost-eyes`, `ghost-hands` workspace crates
- `image` / `imageproc` for capture and processing; optional `ort` (ONNX Runtime) for vision models
- Windows API bindings (`windows`) for window management

## Run standalone

```bash
# From this directory
cargo build --release           # produces the `ghost` binary in target/release

# Optional: enable ONNX-backed vision models
cargo build --release --features ort

./target/release/ghost          # a stdio MCP server — speaks JSON-RPC on stdin/stdout
```

Ghost is a stdio MCP server, so it has no network port. Launch it from any MCP-capable client (or let Ryu Core spawn it as a sidecar). Configuration and cache live under the Ryu data dir: `services/ghost/` (recipes, snapshots) and `models/vision/ghost/` (the vision model).

## What it does

- **Screen perception** — capture, OCR text location, and UI element detection
- **Input control** — click, type, scroll, drag, and wait-for-condition
- **Annotation + recipes** — visual markers and replayable action sequences
- **Multi-monitor** capture with region selection

## Action and error contract

- **Refs.** `ghost_snapshot` renders bare `@eN` refs and returns `ref_prefix` (`@<sid>:`); the qualified `@<sid>:eN` form is preferred and resolves only against that snapshot. A bare ref resolves against the latest snapshot (reported as `ref_resolved_against`) and is refused when a different identified agent took it. Resolution is strict (path, then identity; never values): a gone, changed or ambiguous element fails with `STALE_REF` or `AMBIGUOUS_TARGET` instead of being guessed.
- **Semantic actions.** `ghost_click` takes `action` (`press`, `focus`, `toggle`, `check`, `uncheck`, `expand`, `collapse`, `select`, `scroll_into_view`, `show_menu`) and runs it through AX (macOS), UI Automation (Windows) or AT-SPI (Linux) first. Mouse/keyboard input is a fallback only after an unsupported semantic action, when `allow_hid` is true and the session is not headless; it is reported as `via: "hid"` with a `fallback_reason`. An element already in the requested state succeeds with delivery `not_delivered` and verification `already_in_state`. `ghost_type` adds `replace` (set the value semantically, verified) and `verify` (read back after typing, refs only).
- **Errors.** A failed call returns `isError: true`, text `Error: <CODE>: <message>` with exactly one code, and `structuredContent.error` = `{code, message, disposition: {delivery, retry}, suggestion?, details?}`. Retry is derived from delivery: `not_delivered` is `safe`, `unknown` is `unknown`, anything that may have reached the app is `unsafe`.
- **Safety.** Secure (password) fields carry `secure: true` and `value_withheld: true` and never expose a value, in output or in matching. Typed text is never echoed (`typed_chars` is a count). `ghost_press`/`ghost_hotkey` refuse session-ending and desktop-hijacking shortcuts (quit, log out, lock, app switch, VT switch, ...) with `POLICY_DENIED` before focusing any app, unless `force` is set.
- **Snapshot storage.** Snapshot files are sealed at rest with the Ghost component key. The snapshot id is the `<sid>` in `@<sid>:eN`, so it is visible to the agent and in tool results; it names a sealed file and carries no content. The sealed body also stores the window title, the walk's completeness and renderer state, each element's child-index `path`, `role_key` and `secure` flag (a secure element's value is never stored), and a `creator` field (the calling agent id, used to refuse a bare `@eN` from a different identified agent). The new fields have serde defaults, so snapshots sealed by earlier builds still open.
- **macOS renderers.** Snapshots and actions turn on the accessibility tree of Chromium and Electron apps when needed and report it as `accessibility_enabled`; a failure never fails the call.

## Dual-use disclosure

Screen perception plus synthetic input control are exactly the capabilities malware wants. Ghost is published open-source precisely so the behaviour is auditable, not opaque. Inside Ryu it runs only behind explicit user consent; if you embed it elsewhere, gate it behind clear consent and treat it as a sensitive capability.

## Credits

Ghost is derived from [Ghost OS](https://github.com/ghostwright/ghost-os) by Ghostwright, which is MIT-licensed. The original copyright and license notice are retained in [NOTICE](./NOTICE).

The snapshot/ref model, the qualified ref format, the structured error contract and the semantic action and verification flow are adapted (and modified) from [agent-desktop](https://github.com/vercel-labs/agent-desktop) by Vercel Labs, which is Apache-2.0-licensed. See [NOTICE](./NOTICE).

## Footprint

<!-- BENCH:START (generated by scripts/benchmark.mjs, do not edit by hand) -->

One self-contained native binary: no interpreter, no runtime, no Electron, no Docker.

| Metric | Value |
| --- | --- |
| Release binary | 12.8 MB |
| Resolved crates (transitive deps) | 428 |
| Source | 3,427 lines of Rust across 16 files |

_Measured on `win32` by `node scripts/benchmark.mjs --build --runtime --write`. Reproduce it yourself._

<!-- BENCH:END -->

## License

Apache-2.0 — see [LICENSE](./LICENSE), with MIT-licensed portions from Ghost OS per [NOTICE](./NOTICE). © 2026 A Major Pte. Ltd.
