# Hand-authored fixture overlay

Responses the recorded fixture files can't provide, served by the fake
server *over* `../api/` (same path layout, status 200). Re-recording with
`scripts/record_fixtures.py` never touches this directory.

- `v1/files/FxFixtureFile000000001/comments.json`: the recorded files have
  no comments. Field set, value formats and conventions are copied from real
  `/v1/files/{key}/comments` responses (1,025 comments across 4 files,
  2026-10-03): heads have `parent_id: ""` and a string `order_id`;
  replies have `client_meta: null` and `order_id: null`; only heads carry
  `resolved_at`; newest first. Anchors cover each observed shape:
  node+offset, node+offset+`stable_path`, and node+offset+region+
  `comment_pin_corner`. Also a hidden-layer anchor and a stale anchor
  (deleted node). Node ids point at nodes kept in the trimmed recording.
- `v1/teams/2002/{components,component_sets,styles}.json`: the recorded
  team catalog is empty (the fixture folder's components live in library
  files outside it). Entry shape follows the team-library endpoints
  (`meta.<kind>[]` with `containing_frame`/`containingStateGroup`, single
  page, no cursor). Most entries point at an uncached library file
  (`FxLibraryFile000000003`); "Notifications panel" points at a node in the
  fixture folder so `library search` can show a paste-ready `file:N:x:y`.
  Pagination is tested with per-test `set_route_exact` pages, not here.
