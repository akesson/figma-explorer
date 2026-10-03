# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Cached files are checked against Figma before they're served.** Designers
  edit files while you work, and until now a cached file was served as-is
  until the next `cache prefetch` — days out of date with no warning. Every
  command that reads cached file data now compares the file's Figma `version`
  (a ~1 KB `/meta` request) at most once every 5 minutes per file, and
  refetches a changed file before answering; stderr says
  `cache: N files changed on Figma (…) — refetching…`. Cross-file `find` and
  `ls` at `--depth 2`+ check all the files they read. If the check or the
  refetch fails, the cached copy is served with a note rather than an error.
  `--cache-only` makes no requests and notes data past the 5-minute window.
  The first run after upgrading refetches each file once, since older caches
  don't record a version.

- **Comments are kept current too.** A new comment doesn't change a file's
  Figma `version`, so the check above never saw it, and `comments` kept
  serving whatever the last `cache prefetch` fetched. `comments` and
  `node-info` (when its output includes comments) now re-fetch a file's
  comments if they haven't been checked in the last 5 minutes. If that fails,
  the cached comments are served with a note on stderr, and the next attempt
  waits for the next 5-minute window. `find` and `ls` still read the cached
  comments as-is, since they span every file.

- **Local variables are refreshed with the file.** On accounts with the
  Variables REST API (Enterprise), the cached variables were only re-fetched
  by `cache prefetch`, so after a designer's change `node-info` could pair
  the new design with old variable values. A file that already has cached
  variables now re-fetches them whenever the file itself is refetched. If
  that fails, the old values are kept and `node-info` says on stderr that they
  may be out of date. Files that never had variables (every file on other
  plans) make no extra request.

- **`node-info file:N` now shows each named style's value.** Every entry in
  the file summary's `styles` list carries a `value`: a FILL style's hex (or
  its paint list for gradients and stacked paints), a TEXT style's font
  family, weight, size, line height and letter spacing, an EFFECT style's
  shadow list. Figma returns no values for styles, so each is read off the
  first visible node applying it, offline from the full-file sidecar; a style
  no visible node uses has no `value`. Library copies are flagged
  `remote: true`, since one can share a name with a local style yet differ.
  `node-info file:N --only styles` gives just this table.

### Removed

- **`tokens` is gone.** Its output wasn't usable as design tokens: outside
  `--scope file` it named colors after the layers they were found on
  (`--color-rectangle-13`, `--color-vector`), and it read only fill and text
  styles, so effect styles (shadows, focus rings) never appeared even with
  `--only shadows`. Use instead:
  - a file's named styles and their values → `node-info file:N --only styles`
    (above), which covers effect styles too;
  - the colors, type and effects a frame uses → `node-info file:N:x:y`, which
    lists them with the named style each one comes from.
- **`context` is gone.** It bundled a tree, a screenshot, `tokens` output and
  `assets` into one directory. Run the parts directly: `node-info` for the
  structure, `screenshot --out` for the image, `assets --out-dir` for icons
  and images.

### Fixed

- **macOS binaries are now codesigned and notarized.** Downloading a release
  tarball from GitHub in a browser previously produced "cannot be opened
  because the developer cannot be verified" on first run. Binaries are now
  signed with a Developer ID certificate under the hardened runtime and
  notarized with Apple. Homebrew, npm, and the shell installer were never
  affected (none of them set the quarantine attribute).

## [0.2.3] - 2026-09-23

### Fixed

- **`cache prefetch` now backfills a missing `.full.json.gz` sidecar.** A file
  first cached by opening it via URL (`ls <url>`, `node-info <url>`, …) has a
  payload but no full sidecar, and prefetch used to count it as up to date
  forever — so `node-info --cache-only` never worked offline for it. Prefetch
  now refetches an unchanged file whose full sidecar is missing or stamped
  with another schema version (unless `--no-full`).
- **Root `ls` no longer lists folders with no cached files.** A folder dropped
  from `FIGMA_PROJECTS_IDS` keeps its `proj:N` id (synthetic ids are stable),
  and root `ls` rendered it as an empty project named by its raw folder id
  (e.g. `proj:3 "571382610"`). Such folders are now omitted; `ls proj:N` still
  works, and `cache prefetch --folder-ids <id>` brings a folder back with its
  real name.
- **`cache prefetch`, root `ls` refresh and `ls proj:N` refresh no longer 403
  with a Figma token created after 2026-08-03.** Figma renamed projects to
  folders; new personal access tokens carry `folders:read` instead of
  `projects:read`, and the deprecated `GET /v1/projects/{id}/files` rejects
  them (`Invalid scope … requires the file_read or files:read or projects:read
  scope`). Listings now use `GET /v2/folders/{id}/files`. Folder ids are the
  old project ids, so `FIGMA_PROJECTS_IDS`, `proj:N` and existing cache metas
  are unchanged; `--project-ids` gained a `--folder-ids` alias. A 403 from v2
  falls back to v1 once (pre-rename tokens are documented to keep working
  there) and prints a hint to regenerate the token. `figma-api` was
  regenerated from the 2026-08-11 upstream spec, which adds `folders_api`;
  `figma-get` gained `folder-files`, `folder-folders`, `folder-meta` and
  `team-folders`, and marks `project-files` / `team-projects` deprecated.

### Changed

- **`node-info` output is ~4–5× smaller with nothing implementation-relevant
  dropped.** Measured on a 475-node screen: 634 KB → 141 KB YAML. What changed
  in the curated view (`--raw` is untouched):
  - Hidden (`visible: false`) children are pruned and listed on the parent as
    `hidden_children: [{id, name, type}]` — they were 65% of the output and,
    worse, their descendants were indistinguishable from visible nodes.
    `--include-hidden` restores them; a hidden target is always rendered, and
    so is a hidden layer inside a COMPONENT definition whose visibility is
    wired to a BOOLEAN property (its `property_refs` are the wiring an
    implementer needs). A comment anchored to a pruned node is tagged
    `anchor_rendered: false`.
  - Defaults are elided: `constraints: LEFT/TOP`, `layout.wrap: NO_WRAP`,
    axis `sizing: AUTO` / `align: MIN`, text alignment LEFT/TOP,
    `line_height_unit: PIXELS`.
  - `bounds` is parent-relative on descendants; flow children of an
    auto-layout parent carry only `width`/`height`. A CANVAS has no box, so
    a page's children are relative to the canvas origin (Figma's absolute
    coordinates). `size` appears only when it differs from the bounding box
    (rotation).
  - Colors are `hex` only; the float channels are gone from fills, strokes,
    gradient stops and effects.
  - Text style drops `font_post_script_name`, `line_height_percent`,
    `line_height_percent_font_size` (all restate other fields).
  - `layout_child.sizing` is one `"H/V"` string; padding is one number when
    uniform, else `[top, right, bottom, left]`.
  - `component.component_properties` is split into `variants` and
    `properties` with plain scalar values and Figma's `#n:m` name suffixes
    stripped (kept when stripping would collide). An INSTANCE_SWAP prop keeps
    its allowed swap targets as `preferred`. `property_refs` is emitted on
    the target and inside component definitions only.
  - Geometry leaves below the target (VECTOR, BOOLEAN_OPERATION, STAR, LINE,
    REGULAR_POLYGON) drop only the layout block that cannot apply to them —
    `constraints` when they are flow children of an auto-layout parent,
    `layout_child` when the parent has no auto-layout — so a FILL-width
    divider LINE keeps its sizing; fills and strokes stay so icon color
    tokens remain visible.
  - Bound variables are referenced by short handles (`v1`, `v2`, … in
    first-seen order). The top-level `variables` block is keyed by handle and
    always carries the raw `id`, plus name/values when the Variables sidecar
    is available. The 60-character `VariableID:` strings no longer repeat
    per use.
  - Node-level `bound_variables` no longer repeats `fills[i]`/`strokes[i]`
    entries that the paint itself already carries as `bound_variable` (half
    of the map on a real screen). Entries the paints don't mirror are kept.
  - Floats are rounded to 3 decimals (`14.40007495880127` → `14.4`): Figma
    stores f32 and the REST API prints it as f64. The top-level `variables`
    block gets the same treatment.
  - `--no-variables` once again omits the `variables` block entirely; when a
    sidecar is present but some handles point at library-owned variables, a
    `variables_note` says how many could not be resolved locally.
- **YAML output uses flow style for leaf containers** (`bounds: {width: 24,
  height: 24}`, `padding: [0, 12, 0, 12]`, one record per row in lists)
  instead of `serde_yaml`'s all-block rendering, via the new `yaml_out`
  printer. Still standard YAML; a `node-info` view goes from 4,564 to 2,464
  lines and 31.3k to 27.8k tokens. Non-ASCII keys are no longer escaped
  (`💠 Before Icon`, not `\u{1f4a0} Before Icon`) and integral floats print
  as `12`, not `12.0`. Multi-line text renders as a `|-` literal block.
  Quoting is conservative enough for YAML 1.1 parsers (PyYAML, Psych):
  `687:45` and `10:5` are quoted (base 60 would read them as integers),
  `1,000` and `2026-9-5` too, large floats print as `1.5e+20`, a leading
  `:`/`?` is quoted inside `{}`/`[]`, and U+2028/U+2029, NEL, DEL and the C1
  range are `\uXXXX`-escaped rather than written raw (libyaml refuses the
  whole document otherwise — Shift+Enter in a Figma text layer produces
  U+2028). Applies to every command's YAML output.
- **`--json` is compact**, not pretty-printed. Pretty JSON was the most
  token-expensive rendering of all (37% more than the YAML default on the
  same view); `jq` does not care.
- `scripts/node_info_lossless.py` — checks a curated view against `--raw` for
  the same target (node set, hidden listing, bounds reconstruction, paints,
  layout, text, component props, variable handles).

## [0.2.2] - 2026-08-22

### Fixed

- **npm package for 0.2.1 was never published** — the OIDC publish job used a
  placeholder auth token written by `actions/setup-node`. Fixed; 0.2.2 is the
  first release on npm via Trusted Publishing. No CLI changes.

## [0.2.1] - 2026-08-22

### Changed

- **npm releases now publish via npm Trusted Publishing (OIDC)** instead of a
  long-lived `NPM_TOKEN`. Packages carry provenance attestations. No
  user-facing changes to the CLI.

## [0.2.0] - 2026-07-29

### Added

- **Web-search query syntax for `find`** — the syntax every agent already
  knows from Google/GitHub: bare words stay fuzzy tokens with implicit AND
  (unchanged); `"quoted phrases"` require the exact contiguous text
  (case-insensitive) — the lane for hunting rendered copy like
  `find '"Approved by you"'`; uppercase `OR` alternates adjacent terms
  (`a b OR c` = a AND (b OR c)); `-term` / `-"phrase"` exclude any chain
  containing them; uppercase `AND` is accepted as a no-op. Lowercase
  `or`/`and` remain ordinary tokens, and mid-word hyphens (`night-shift`)
  stay literal. Exact-phrase hits score above same-length fuzzy matches, so
  copy searches rank the real thing first. Note the shell eats outer
  quotes: write `find '"exact phrase" context'`.

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
