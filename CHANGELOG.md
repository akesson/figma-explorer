# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
