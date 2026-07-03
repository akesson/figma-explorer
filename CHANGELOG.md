# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `find` now matches a node's **visible text**, not just its layer name. TEXT
  content (`characters`) is captured into the structural cache (truncated to
  160 chars) so a query like `leave details` finds the button whose layer is
  named "Button Label" but whose copy reads "Leave details". Text-lane matches
  render a `text:"…"` snippet line under the hit (JSON: `text_matches`), and
  unnamed TEXT nodes with copy are now searchable. This bumps
  `CACHE_SCHEMA_VERSION` to 2 — existing caches silently refetch on next
  access; under `--cache-only`, run `cache prefetch` once first.
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

### Fixed

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
