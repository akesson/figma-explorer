---
name: figma-explorer
description: Inspect Figma files from the CLI — list/search nodes, dump trees, extract design tokens, export screenshots and assets. Default output is compact YAML, designed to be read by Claude Code.
---

# figma-explorer

CLI on top of the Figma REST API. Default to this over the Figma MCP server when you need bulk navigation, search, comments, or token/asset extraction — it's faster, locally cached, and pastable.

Binary: `figma-explorer` (on PATH). Needs `FIGMA_TOKEN` in env or `.env`.

## Tagged-ID grammar

Every positional `<ID>` accepts:

- `proj:N` — synthetic project id (small, stable across runs)
- `file:N` — synthetic file id
- `file:N:x:y` — a node inside that file (Figma's native `x:y` form)
- `file:N:comm:M` — a comment thread
- `x:y` — bare native node id (ambiguous; pair with `--in file:N`)
- a full `figma.com/design/...?node-id=...` URL

Synthetic ids (`proj:N`, `file:N`) are assigned once and persisted; you can paste them between commands and across sessions. Discover them with `figma-explorer ls` (no args) — it lists root projects/files.

## Commands

```
ls          [ID]   tree under a node (or root). --depth N, --no-ignore, --resolved
find        QUERY  fuzzy multi-token ancestor-chain search. --in, --type, --limit
comments    [ID]   comments pinned to a node/file. --resolved, --max-age-secs
screenshot  ID     export PNG/JPG/SVG/PDF. --out, --scale, --img-format
tokens      ID     design tokens. --as tokens|css|tailwind, --only colors,..., --scope
assets      ID     bulk SVG/PNG export under a subtree. --out-dir
context     ID     bundle: tree.txt + screenshot.png + styles/ + assets/. --out-dir
                   tree.txt uses the same flat pipe-rail format as ls/find
cache       prefetch | clear [--file-key K]
```

Global flags that apply everywhere: `--json` (else compact text format), `--cache-only` (no live fetches), `--in <ID>` (scope; `find` searches inside it, `ls` uses it to qualify a bare native id like `0:0`).

## How to use it

1. **Discover.** `figma-explorer ls` to see projects and top-level files. Note the `proj:N` / `file:N` ids.
2. **Drill in.** `figma-explorer ls file:N --depth 1` to see canvases. `--no-ignore` reveals hidden Cover/WIP/Archive canvases (filtered by default).
3. **Search.** `figma-explorer find "employee status" --in file:28 --limit 5`. Tokens are whitespace-split; each must fuzzy-match some ancestor in the chain. Results are scored — higher score = each token landed on a more distinct ancestor.
4. **Pull context.** For a node you care about, `figma-explorer context file:28:2974:150299 --out-dir /tmp/foo` writes `tree.txt`, `screenshot.png`, `styles/tokens.{json,css}`, `styles/tailwind.json`, and `assets/{icons,images,composites}/`.
5. **Comments.** `figma-explorer comments file:28 --resolved false` to triage. Each row shows pin status (`explicit` vs `stale-ref`) and the node it points at, so you can jump back via `ls`/`screenshot`.

## Output style

- Default is compact YAML on stdout — one indented line per node with `<id>  <bounds>@<x,y>  | <TYPE>  "<name>"`. Ideal for grepping or feeding back into another command.
- `--json` emits pretty JSON when you want structured data.
- `screenshot` without `--out` prints the rendered S3 URL (cheap; no download).
- `find` prints `# showing N of M matches — use --limit N to see more` when truncated.

## Tips

- For bare node ids (`x:y`) from a URL or designer DM, always pair with `--in file:N` to avoid ambiguity across files.
- `--cache-only` is the right default for read-heavy automation; let `cache prefetch` populate first.
- Cache lives at `$FIGMA_EXPLORER_CACHE_DIR` or `dirs::cache_dir()`. `cache clear --file-key <key>` for surgical invalidation; `cache clear` wipes everything.
- `tokens --scope target` restricts to the resolved subtree's actually-used values; `--scope file` is only the published library styles; `both` (default) unions them.
- `assets` separates flat SVG icons from PNGs from "composite" PNGs (subtrees that don't fit one image format). Check the output summary for counts and failures.
- Comments are cached on disk; `--max-age-secs 0` forces a refetch when you need the latest.
