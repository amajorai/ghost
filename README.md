# Ghost

> Eyes and hands for AI agents on the desktop — a Model Context Protocol (MCP) server for
> screen perception and input control.

[![License](https://shieldcn.dev/badge/License-Apache--2.0-73DC8C.svg?logo=apache&logoColor=white)](./LICENSE)
[![Discord](https://shieldcn.dev/discord/1439211418724597800.svg?logo=discord&logoColor=white&color=4B78E6)](https://ryuhq.com/discord)

Ghost is a Rust MCP server (~30 tools) that gives an agent the ability to *see* the screen (capture,
OCR, UI-element detection) and *act* on it (click, type, scroll, drag), plus a record→replay recipe
engine. Windows-first. It's a component of [Ryu](https://github.com/amajorai/ryu) but runs standalone
with any MCP-capable client.

- 👁️ **See** — screen capture, OCR, and UI-element detection over the accessibility tree.
- ✋ **Act** — click, type, scroll, and drag through synthetic keyboard/mouse input.
- 🎬 **Record → replay** — capture a task once, replay it deterministically as a parameterized recipe.
- 🧩 **~30 MCP tools** — drop into any MCP client, or let Ryu Core spawn it as a sidecar. Stdio, no network port.
- 🪟 **Windows-first** — with cross-platform (macOS/Linux) perception + input backends in progress.
- 🔓 **Open & auditable** — dual-use by nature, so the behaviour is open source and, inside Ryu, consent-gated.

## Layout

| Path | What |
|---|---|
| `apps/ghost` | the MCP server binary |
| `crates/ghost-core` | core automation primitives (recipes, store) |
| `crates/ghost-eyes` | screen perception / vision |
| `crates/ghost-hands` | synthetic keyboard/mouse input |

## Build

```bash
cd apps/ghost && cargo build --release   # → a stdio MCP server (no network port)
```

Launch it from any MCP client, or let [Ryu Core](https://github.com/amajorai/ryu) spawn it as a
sidecar. Config + cache live under `~/.ghost/`.

## Dual-use & consent

Screen perception + synthetic input control are exactly what malware wants. Ghost is open-source so
the behaviour is auditable, and inside Ryu it runs only behind explicit user consent. If you embed
it, gate it behind clear consent and treat it as a high-trust dependency. See [SECURITY](./apps/ghost/SECURITY.md).

## Credits & license

Ghost is derived from [Ghost OS](https://github.com/ghostwright/ghost-os) by Ghostwright (MIT) — the
original copyright + license notice are retained in [NOTICE](./NOTICE). Ghost is licensed under
**Apache-2.0** (see [LICENSE](./LICENSE)) with MIT-licensed portions per NOTICE. © 2026 A Major Pte. Ltd.
