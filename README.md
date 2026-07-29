# figma-explorer

A command-line tool for navigating, inspecting, and exporting from Figma files via the REST API. Built for use alongside AI coding agents that need to consume design context as text and assets.

## What it does

- **`ls`** / **`find`** — navigate projects, files, and node trees by name or visible text without copy-pasting node ids. `find` speaks web-search syntax: `"exact phrases"`, uppercase `OR`, `-exclusion`.
- **`node-info`** — single-target view with layout, fills, strokes, effects, text, bound variables, comments, and a ready-made figma.com `url:`. Designed to be pasted into an agent prompt.
- **`comments`** — list a file's comment threads (replies inline, newest first), threads under a node, or a single thread; filter with `--unresolved` / `--since` / `--grep`.
- **`mark`** — curated keyword→node bookmarks (`mark add key file:N:x:y --alias …`); `find` and `library search` surface matching marks ahead of their own hits, and `mark:key` resolves anywhere an id is accepted.
- **`library search`** — fuzzy search across a team's published component library (components, component sets, styles).
- **`screenshot`** — export a node as PNG / JPG / SVG / PDF.
- **`tokens`** — extract design tokens (colors, fonts, sizes, spacing).
- **`assets`** — bulk-export every icon/image below a node.
- **`context`** — aggregate: tree + screenshot + tokens + assets for a node.
- **`cache prefetch`** / **`cache status`** — pre-warm a local cache so subsequent commands are offline-fast, and report what it holds (per-file age, sidecars, catalog) without touching the network.

All commands share a tagged-id grammar — `proj:N`, `file:N`, `file:N:x:y`, or a full `figma.com` URL — so you never paste raw node identifiers by hand.

## Install

### macOS (Homebrew)

```sh
brew install akesson/tap/figma-explorer
```

### Linux / macOS (shell installer)

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/akesson/figma-explorer/releases/latest/download/figma-explorer-installer.sh | sh
```

### Windows (PowerShell)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/akesson/figma-explorer/releases/latest/download/figma-explorer-installer.ps1 | iex"
```

### npm

```sh
npm install -g @akesson/figma-explorer
```

### From source

```sh
cargo install --git https://github.com/akesson/figma-explorer figma-explorer
```

## Setup

Get a personal access token at https://www.figma.com/developers/api#access-tokens and export it:

```sh
export FIGMA_TOKEN=figd_...
```

Optional environment variables:

- `FIGMA_PROJECTS_IDS` — comma-separated project ids; needed by `cache prefetch`.
- `FIGMA_TEAM_ID` — needed by `library search` and for the catalog warm in `cache prefetch`.
- `FIGMA_EXPLORER_CACHE_DIR` — override the cache location (default: OS cache dir).

A `.env` file in the current directory (or any ancestor up to root) is auto-loaded, with `~/.config/figma-explorer/.env` as the global fallback — handy for git worktrees and directories with no `.env` in their ancestry. Exported environment variables always win.

## Quickstart

```sh
# List everything at the root
figma-explorer ls

# Drill into a project, then a file
figma-explorer ls proj:1
figma-explorer ls file:2

# Find a node by name across every cached file
figma-explorer find "Button / Primary"

# Hunt rendered copy with an exact phrase (single-quote so the shell keeps the double quotes)
figma-explorer find '"Approved by you"'

# ...or scope the search to one file
figma-explorer --in file:2 find "Button / Primary"

# Get implementation-ready info for one node
figma-explorer node-info file:2:1094:66591

# Search the team component library
figma-explorer library search "icon arrow"

# Export everything below a node as PNGs
figma-explorer assets file:2:1094:66591 ./out/

# Pre-warm the cache so subsequent reads are offline
figma-explorer cache prefetch
```

## Output

YAML by default (terse, agent-friendly). Pass `--json` for full pretty JSON suitable for `jq` pipelines.

## Cache

A local rkyv cache lives at `$FIGMA_EXPLORER_CACHE_DIR` or the OS cache dir. It is **per-machine** — rkyv archives are not endianness-portable, so don't sync the cache directory across architectures. `cache clear` removes everything; `cache prefetch` warms it from `FIGMA_PROJECTS_IDS`.

## License

MIT. See `LICENSE-MIT`.
