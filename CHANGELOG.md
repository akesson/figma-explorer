# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `cache status` — offline report of what the cache holds: per-file rows
  (synth id, name, key, project, payload age, node count) with sidecar
  presence/age for full/comments/variables, plus totals, team-catalog state,
  and mark count. Ends agents introspecting the cache directory by hand (and
  getting it wrong).
- `node-info` now emits a ready-made figma.com `url:` — on the `target` block
  for node targets (deep link with `node-id=`) and on every `file` block —
  so reports can link straight into Figma without hand-assembling URLs from
  the raw `key`.
- `--only` now works on **file** targets too: `meta` (just the counts),
  `pages`, `component`, `styles`, `variables`, `comments` filter the file
  summary. Node-only sections on a file target (and the new `pages` section
  on a node target) are rejected with a hint instead of silently emitting
  nothing.

- **Marks** — a curated keyword→node database (`mark add`/`rm`/`list`, and a new
  `mark:<key>` id). Once you've positively identified a node, `mark add <key>
  <ID> [--alias …] [--note …]` writes the mapping down so the expensive
  discovery never repeats. `find` and `library search` now fold matching marks
  in **ahead of** their own hits, so a query in *your* vocabulary ("leave
  tooltip") surfaces the node even when no layer name matches. `mark:<key>`
  resolves like the underlying node, so `node-info mark:k`, `screenshot mark:k`,
  and `--in mark:k` work transparently; a multi-node mark lists its paste-ready
  ids so you can pick one. Marks live in `<cache-root>/marks.json` (beside
  `synth.json`) and **survive `cache clear`**. Each mark node carries a stamp of
  the node's name + ancestor path when added, so `mark list` flags drift as
  `[renamed]` / `[moved]` / `[gone]` / `[uncached]` rather than silently
  pointing at a node the design moved out from under.
- `find` now matches a node's **visible text**, not just its layer name. TEXT
  content (`characters`) is captured into the structural cache (truncated to
  160 chars) so a query like `leave details` finds the button whose layer is
  named "Button Label" but whose copy reads "Leave details". Text-lane matches
  render a `text:"…"` snippet line under the hit (JSON: `text_matches`), and
  unnamed TEXT nodes with copy are now searchable. This bumps
  `CACHE_SCHEMA_VERSION` to 2 — existing caches silently refetch on next
  access; under `--cache-only`, run `cache prefetch` once first.
- `comments <ID> --grep <PATTERN>` — case-insensitive substring filter over
  thread head + reply messages. Composes with `--unresolved`/`--since`/
  `--limit`; the header reports `# N of M threads match "<pattern>"` (JSON
  summary gains `grep`).
- `find` now surfaces a comment-mention hint: after the search it reports
  which searched files discuss the query in their comment threads
  (`# N comment threads mention "tooltip" — try: comments file:15 --grep
  "tooltip"`, capped at 3 files; JSON: `comment_mentions`). A name-search
  miss often lands in the designers' discussion, which is written in user
  vocabulary — this closes that dead end.
- `ls --comments` — restore the pre-diet inline comment thread rows. By
  default `ls` now summarizes comments (see Changed).
- `comments <ID>` — list every comment thread in a file (replies inline,
  sorted newest-activity-first, full message text), threads anchored under a
  node subtree, or one thread by `file:N:comm:M`. Filters: `--unresolved`,
  `--since <ISO8601>` (prefix-friendly, matches head or reply activity),
  `--limit N`. `--refresh` re-fetches a single file's comments — no full
  `cache prefetch` needed. Previously only `node-info` exposed comments,
  capped at 10 recent threads.
- `node-info` file targets now sort `recent_comments` newest-first (was:
  API order) and add a `comments_hint` pointing at `comments file:N` when
  more threads exist than the summary shows.
- `node-info --only <sections>` — restrict output to named sections
  (`fills,strokes,effects,geometry,corner,layout,text,component,prototype,meta,styles,variables,comments`)
  instead of piping the full dump through grep. Identity (id/type/name) is
  always emitted; the hoisted top-level `variables`/`styles_index` blocks
  keep exactly the entries referenced by kept sections. `--only prototype`
  and `--only meta` imply those opt-in sections.
- `ls --name <PATTERN>` — case-insensitive substring filter over node names.
  Matches keep their ancestor rail for tree context; other branches are
  pruned; root/project listings drop files/projects with no matches inside
  (unless their own name matches). A `# name filter "…": N matches` line
  (JSON: `name_filter` object) makes the filtering visible.
- `find` now prints `# searched N cached files` on unscoped runs (JSON:
  `searched_files`) — cross-file search has always been the no-`--in`
  default, but nothing said so; help text and docs now do, and zero-match
  runs are no longer silent.
- `FIGMA_TOKEN` (and any other env) now falls back to a global
  `$XDG_CONFIG_HOME`-or-`~/.config/figma-explorer/.env`, loaded after the
  cwd-upward `.env` walk (lowest priority). Covers git worktrees that don't
  see the canonical checkout's `.env`.

- `library search <query>` — fuzzy text search across a team's published
  design-system catalog (components, component sets, and styles). Each hit
  reports the component/style key and a paste-ready `file:N:x:y` id when the
  source file is known to the cache. Supports `--type`, `--limit`, and
  `--refresh`. The catalog is fetched from the team-library REST endpoints and
  cached as a team-scoped sidecar (`teams/{team_id}.catalog.json.gz`),
  refreshed lazily on a 24h TTL. Variables are not indexed — the Figma
  Variables REST API is Enterprise-gated.
- `cache prefetch` now warms the team-library catalog so
  `library search --cache-only` works offline; skip it with `--no-catalog`.
- `FIGMA_TEAM_ID` environment variable (also `--team-id`), consumed by
  `library search` and the `cache prefetch` catalog warm.

### Changed

- Root `ls` (no ID) now defaults to depth 1 — projects + files only — instead
  of descending into every file's canvases/frames. A full workspace dump
  measured ~237KB; the shallow default is ~2KB with a `# depth 1 …` hint on
  how to descend (`ls file:N`, `ls proj:N`, or `--depth 2`). Explicit
  `--depth` is unchanged; every non-root target still defaults to depth 3.
  JSON root output is also depth 1 by default now — pass `--depth 3` for the
  old shape.
- `ls` now summarizes comments by default instead of interleaving every
  thread: a node with anchored threads shows a `[N comments]` suffix, and
  file targets get a `# N comment threads (M unresolved) — use: comments
  file:N …` header. Pass `--comments` for the old inline rows. Because the
  filter now applies to those rows, `--resolved` requires `--comments`. JSON
  output is unchanged (full comment arrays regardless).
- `library search` now distinguishes strong from weak fuzzy matches: when any
  hit clears ~85% of the query's self-score, only strong hits show (with a
  `# N weaker matches hidden` line); when none do, it prints `# no strong
  match for "<q>"` and lists a few `(weak)`-labeled leads instead of ranking
  subsequence junk as if it were relevant. JSON hits gain `strong`; the
  envelope gains `no_strong_match`/`self_score`.

### Fixed

- Piping any command into a reader that closes early (`figma-explorer ls … |
  head`) no longer panics with `failed printing to stdout: Broken pipe (os
  error 32)`. The Rust runtime ignores `SIGPIPE` at startup, which turned a
  broken pipe into a `println!` panic (exit 101); both binaries now restore the
  default disposition so the process ends quietly (exit 141) like a normal Unix
  CLI. Matters most for agents, which pipe through `head`/`grep` constantly.
- `--cache-only` is now enforced by the live-fetch commands (`tokens`,
  `screenshot`, `assets`, `context`). Previously the flag was honored only
  during id resolution, then the mandatory step-2 live fetch proceeded anyway —
  so `--cache-only tokens …` silently hit the network. These commands cannot be
  served offline (they need fills/strokes/type styles or the `/images` API), so
  they now bail up front with a clear message, matching `comments --refresh`.
- Root `ls` on a cache with no files (fresh, or just cleared) now prints a
  `no cached files — run … cache prefetch …` nudge (and a `hint` key in
  `--json`) instead of only the depth hint, which gave no clue the cache needed
  populating.
- Instance-descendant node ids (`I880:3606;2816:36646`) are now accepted
  everywhere an id is: qualified (`file:7:I880:3606;2816:36646`), bare (with
  `--in`), and in figma.com URLs (`node-id=I880-3606%3B2816-36646`).
  Previously `ls`/`node-info` printed these ids but the parser rejected them
  ("node part is not NUM:NUM"), so the CLI's own output couldn't be pasted
  back into `node-info`/`screenshot`.
- Tagged file/node targets (`file:N`, `file:N:x:y`) now cold-fetch a missing
  or evicted cache entry instead of dead-ending with "nothing cached … (no
  meta on disk)", matching the URL lane's behavior. Under `--cache-only` the
  miss reports the standard remedy hint instead. The residual disk-only error
  message now also names `cache prefetch` and the URL alternative.
- `cache clear` (without `--file-key`) swept only the `files/` directory,
  silently leaving other cache state on disk; it now also clears the
  team-catalog sidecars under `teams/`. The command's help text overstated its
  scope and has been corrected.
- A cached payload written under an older `CACHE_SCHEMA_VERSION` now resolves as
  a cache miss (→ live refetch, or a clean `--cache-only` miss) instead of a
  hard "cache schema version mismatch" internal error. The version check
  already promised silent refetch, but the tagged file/node resolve lane
  mapped the mismatch to an internal error — so a schema bump would have
  dead-ended every synth-id lookup until a manual `cache clear`.
