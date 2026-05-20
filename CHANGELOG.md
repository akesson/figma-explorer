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

- `cache clear` (without `--file-key`) swept only the `files/` directory,
  silently leaving other cache state on disk; it now also clears the
  team-catalog sidecars under `teams/`. The command's help text overstated its
  scope and has been corrected.
