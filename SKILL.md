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
node-info   [ID]   curated single-target view: layout, fills, effects, text, component
                   metadata, bound variables, anchored comments. Accepts node, comment,
                   file, project, and root targets. See "Design-to-code" below.
screenshot  ID     export PNG/JPG/SVG/PDF. --out, --scale, --img-format
tokens      ID     design tokens. --as tokens|css|tailwind, --only colors,..., --scope
assets      ID     bulk SVG/PNG export under a subtree. --out-dir
context     ID     bundle: tree.txt + screenshot.png + styles/ + assets/. --out-dir
                   tree.txt uses the same flat pipe-rail format as ls/find
cache       prefetch [--no-full|--no-variables|--force] | clear [--file-key K]
```

Global flags that apply everywhere: `--json` (else compact text format), `--cache-only` (no live fetches), `--in <ID>` (scope; `find` searches inside it, `ls` uses it to qualify a bare native id like `0:0`).

## How to use it

1. **Discover.** `figma-explorer ls` to see projects and top-level files. Note the `proj:N` / `file:N` ids.
2. **Drill in.** `figma-explorer ls file:N --depth 1` to see canvases. `--no-ignore` reveals hidden Cover/WIP/Archive canvases (filtered by default).
3. **Search.** `figma-explorer find "employee status" --in file:28 --limit 5`. Tokens are whitespace-split; each must fuzzy-match some ancestor in the chain. Results are scored — higher score = each token landed on a more distinct ancestor.
4. **Implement a frame.** `figma-explorer node-info file:28:2974:150299` for a one-shot LLM-friendly view (everything you need to write the JSX/CSS in one read). For visual reference + bulk assets too, use `context` instead — it bundles a screenshot + token files + an `assets/` directory.
5. **Comments.** `figma-explorer comments file:28 --resolved false` to triage. Each row shows pin status (`explicit` vs `stale-ref`) and the node it points at. To pull a single thread (parent + replies in one shot), use `node-info file:28:comm:M`.

## Design-to-code with `node-info`

`node-info` is the right command when you're translating a Figma node to application code. Output shape is uniform across target kinds:

```
target: { kind: node|comment|file|project|root, id, path: [{id,type,name}, ...] }
file:   { key, name, synth, last_modified, ... }
node:   id, type, name, bounds, constraints, corner?, fills?, strokes?, stroke?,
        effects?, layout?, layout_child?, text?, component?, prototype?,
        styles?, bound_variables?, comments?, children?  (snake_case throughout)
variables:    { <VariableID>: { name, collection, resolved_type, values_by_mode, scopes } }
styles_index: { <S:id>: { key, name, description, type } }
truncated:    { reason, omitted_count, omitted_node_ids, hint }  (only when capped)
```

Defaults are tuned for "useful but not overwhelming":
- Empty arrays / default values (`visible: true`, `rotation: 0`, `opacity: 1`) are omitted.
- Children: full subtree by default, capped at `--max-nodes 500`; per-node detail tiers down past depth 0 (effects/prototype/meta drop from descendants).
- Variables and named styles are **hoisted to top-level blocks** and referenced by id from each node — no duplication of token data across 40 children.
- Comments anchored to the target or its subtree are inlined under `node.comments`. Pass `--no-comments` to skip.

Useful flags:
- `--depth N` / `--no-children` — limit subtree depth.
- `--max-nodes N` — emit a `truncated` block instead of dumping huge frames.
- `--prototype` — include interactions/transitions (off by default).
- `--meta` — include `dev_status`, `annotations`, `export_settings` (off by default).
- `--rich-text` — emit per-character-range `text.overrides` for inline links/colored spans.
- `--no-variables` — skip the hoisted `variables` block.
- `--raw` — escape hatch: dump the raw Figma JSON (camelCase) for the target. Use when the curated view drops something you need.

Variables: requires the paid-tier Variables REST API. `cache prefetch` adaptively disables variables fetching after 3 consecutive 403s; non-Enterprise accounts will see `variables_disabled: true` in the prefetch summary and no top-level `variables` block in `node-info` output. Set `FIGMA_EXPLORER_FETCH_VARIABLES=0` to opt out entirely.

## Output style

- Default is compact YAML on stdout — one indented line per node with `<id>  <bounds>@<x,y>  | <TYPE>  "<name>"`. Ideal for grepping or feeding back into another command.
- `--json` emits pretty JSON when you want structured data.
- `screenshot` without `--out` prints the rendered S3 URL (cheap; no download).
- `find` prints `# showing N of M matches — use --limit N to see more` when truncated.

## Tips

- For bare node ids (`x:y`) from a URL or designer DM, always pair with `--in file:N` to avoid ambiguity across files.
- `--cache-only` is the right default for read-heavy automation; let `cache prefetch` populate first. `node-info` honors this strictly — a missing sidecar errors with a "run cache prefetch" hint instead of silently hitting the network.
- Cache lives at `$FIGMA_EXPLORER_CACHE_DIR` or `dirs::cache_dir()`. `cache clear --file-key <key>` for surgical invalidation; `cache clear` wipes everything. Per file, the cache holds: `<key>.rkyv` (structural tree for ls/find), `<key>.meta.json` (status), `<key>.comments.json` (pre-associated comments), `<key>.full.json.gz` (raw `/v1/files` body for `node-info`), and `<key>.variables.json` when the account has Variables API access.
- `tokens --scope target` restricts to the resolved subtree's actually-used values; `--scope file` is only the published library styles; `both` (default) unions them.
- `assets` separates flat SVG icons from PNGs from "composite" PNGs (subtrees that don't fit one image format). Check the output summary for counts and failures.
- Comments are cached on disk; `--max-age-secs 0` forces a refetch when you need the latest. For a single thread (parent + replies), `node-info file:N:comm:M` is the one-shot way to read it.
- Only `node-info` accepts comment ids; `ls`/`screenshot`/`tokens`/`context`/`assets` reject `file:N:comm:M` with a hint pointing at `node-info`.
