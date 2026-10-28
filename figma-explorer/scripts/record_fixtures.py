#!/usr/bin/env python3
"""Record scrubbed Figma API responses for the e2e suite (`tests/e2e`).

Fetches every endpoint the CLI reads for a dedicated fixture folder, scrubs
anything identifying, and writes the bodies under `tests/fixtures/api/` plus a
`routes.json` index the fake server (`tests/e2e/fake_figma.rs`) serves from.

Inputs (environment, or a `.env` in the repo root):
  FIGMA_TOKEN      token with file_content/file_metadata/file_comments/
                   projects(folders)/team_library_content read scopes
  REC_FOLDER_ID    folder (Figma "project") holding the fixture files; the
                   recorded listing is filtered down to REC_FILE_KEYS
  REC_FILE_KEYS    comma-separated file keys inside that folder, in a fixed
                   order — they become FxFixtureFile000000001, …02, …
  REC_TEAM_ID      optional: team whose published library holds the fixture
                   components. Only entries whose file_key is one of
                   REC_FILE_KEYS are kept, so the rest of the library never
                   lands in the repo.
  REC_REDACT       optional `real=fake;…` substitutions for names or copy in
                   the designs that aren't user handles. Keep it in the
                   gitignored `.env` — the mapping itself names the real people.

File bodies are trimmed to a feature-covering subset (see `trim_document`):
every node kind, paint/effect/layout/variable/style/component-property shape
found in the source files survives, the rest of the tree doesn't.

Scrubbing: real file keys, folder id and team id are replaced by fixed fakes
(string replace across every body); every user object becomes a fixture
user; emails and signed thumbnail/image URLs become placeholders. After
writing, every body is grepped for the original identifiers and the script
fails if any survived.

Usage:  scripts/record_fixtures.py           record (overwrites fixtures)
        scripts/record_fixtures.py --check   re-fetch, diff against the
                                             committed fixtures, exit 1 on drift
"""

import argparse
import difflib
import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

API = "https://api.figma.com"
CRATE = Path(__file__).resolve().parent.parent
OUT = CRATE / "tests" / "fixtures" / "api"
FAKE_FOLDER = "1001"
FAKE_TEAM = "2002"
EMAIL_RE = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
PLACEHOLDER_URL = "https://example.invalid/placeholder.png"


def fake_key(i):
    return f"FxFixtureFile{i + 1:09d}"


def load_dotenv():
    env = CRATE.parent / ".env"
    if not env.exists():
        return
    for line in env.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


def get(path, token):
    req = urllib.request.Request(API + path, headers={"X-Figma-Token": token})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        body = e.read()
        try:
            return e.code, json.loads(body)
        except ValueError:
            return e.code, {"raw": body.decode(errors="replace")}


class Scrubber:
    def __init__(self, replacements, redact):
        # Longest first so a key that prefixes another can't half-replace it.
        self.replacements = sorted(replacements.items(), key=lambda kv: -len(kv[0]))
        # Free-text substitutions: ids, user handles (they also show up as
        # text in mock-ups), and REC_REDACT entries.
        self.strings = dict(replacements)
        # REC_REDACT applies per string value: entries of 4+ chars replace as
        # substrings ("Ann Lee" inside a sentence); shorter ones (initials
        # like "AL") only replace a whole value, so "PN" can't hit "PNG".
        self.redact = sorted(redact.items(), key=lambda kv: -len(kv[0]))
        self.users = {}
        self.originals = set(replacements) | set(redact)

    def user(self, u):
        uid = str(u.get("id", ""))
        if uid and uid not in self.users:
            self.users[uid] = f"fixture-user-{len(self.users) + 1}"
            self.originals.add(uid)
        for k in ("handle", "email"):
            if u.get(k):
                self.originals.add(u[k])
        n = self.users.get(uid, "fixture-user-0")
        out = dict(u)
        out["id"] = n
        out["handle"] = n.replace("-", " ").title()
        if u.get("handle"):
            self.strings.setdefault(u["handle"], out["handle"])
        if "img_url" in out:
            out["img_url"] = PLACEHOLDER_URL
        if "email" in out:
            out["email"] = f"{n}@example.invalid"
        return out

    def walk(self, v):
        if isinstance(v, dict):
            if "handle" in v and "id" in v:
                v = self.user(v)
            out = {}
            for k, x in v.items():
                if k in ("thumbnail_url", "thumbnailUrl", "img_url") and isinstance(x, str):
                    out[k] = PLACEHOLDER_URL
                elif k in ("user_id",) and isinstance(x, str):
                    self.originals.add(x)
                    out[k] = self.users.setdefault(x, f"fixture-user-{len(self.users) + 1}")
                else:
                    out[k] = self.walk(x)
            return out
        if isinstance(v, list):
            return [self.walk(x) for x in v]
        if isinstance(v, str):
            for real, fake in self.redact:
                if len(real) >= 4:
                    v = v.replace(real, fake)
                elif v == real:
                    v = fake
            return EMAIL_RE.sub("fixture@example.invalid", v)
        return v

    def text(self, walked):
        """Render a body already passed through `walk`. Call only after every
        body has been walked, so handles first seen in a later body (e.g.
        comments) are still replaced in earlier ones."""
        s = dumps(walked) + "\n"
        for real, fake in sorted(self.strings.items(), key=lambda kv: -len(kv[0])):
            s = s.replace(real, fake)
        return s

    def leaks(self, s):
        return sorted(o for o in self.originals if o and len(o) >= 4 and o in s)


def node_features(n):
    """What a node exercises, as tags. Trimming keeps at least one node per
    tag found in the source files, so every implementation-relevant shape the
    real files contain survives into the fixtures."""
    f = {f"type:{n['type']}"}
    if n.get("visible") is False:
        f.add("hidden")
    if n.get("layoutMode", "NONE") != "NONE":
        f.add(f"layout:{n['layoutMode']}")
    if n.get("layoutWrap") == "WRAP":
        f.add("layoutWrap")
    if n.get("layoutPositioning") == "ABSOLUTE":
        f.add("absolutePositioned")
    for k in ("fills", "strokes", "background"):
        for paint in n.get(k) or []:
            f.add(f"{k}:{paint.get('type')}")
            if paint.get("visible") is False:
                f.add(f"{k}:hiddenPaint")
            if "boundVariables" in paint:
                f.add(f"{k}:boundVar")
    for e in n.get("effects") or []:
        f.add(f"effect:{e.get('type')}")
    f |= {f"boundVariables:{k}" for k in n.get("boundVariables") or {}}
    f |= {f"style:{k}" for k in n.get("styles") or {}}
    f |= {f"componentProp:{p.get('type')}" for p in (n.get("componentProperties") or {}).values()}
    for k in ("componentPropertyDefinitions", "componentPropertyReferences", "overrides",
              "styleOverrideTable", "layoutGrids", "exportSettings", "strokeDashes",
              "individualStrokeWeights", "rectangleCornerRadii", "rotation", "isMask",
              "clipsContent", "interactions", "transitionNodeID", "cornerRadius"):
        if n.get(k):
            f.add(k)
    style = n.get("style") or {}
    f |= {f"text:{k}" for k in ("textCase", "textDecoration", "textAutoResize", "lineHeightUnit",
                                "hyperlink", "textTruncation") if k in style}
    if n.get("blendMode") not in (None, "PASS_THROUGH", "NORMAL"):
        f.add("blendMode")
    if n.get("opacity", 1) != 1:
        f.add("opacity")
    if any(k in n for k in ("minWidth", "maxWidth", "minHeight", "maxHeight")):
        f.add("minMaxSize")
    c = n.get("constraints")
    if c:
        f.add(f"constraint:{c.get('horizontal')}/{c.get('vertical')}")
    for axis in ("Horizontal", "Vertical"):
        if n.get(f"layoutSizing{axis}"):
            f.add(f"sizing{axis[0]}:{n[f'layoutSizing{axis}']}")
    return f


def index_nodes(doc):
    nodes, parent = {}, {}

    def walk(n, p):
        nodes[n["id"]] = n
        parent[n["id"]] = p
        for c in n.get("children", []):
            walk(c, n["id"])

    walk(doc, None)
    return nodes, parent


def compact_size(v):
    return len(json.dumps(v, separators=(",", ":")))


def trim_document(doc, need):
    """Keep the document, every canvas, and for each feature tag in `need` the
    cheapest node carrying it (two per node type) together with its ancestor
    chain. "Cheapest" counts only ancestors not already kept, so features
    cluster inside a few frames instead of dragging in one frame each.
    Kept nodes are verbatim apart from their pruned `children`."""
    nodes, parent = index_nodes(doc)
    feats = {i: node_features(n) for i, n in nodes.items()}
    own = {i: compact_size({k: v for k, v in n.items() if k != "children"}) for i, n in nodes.items()}
    keep = {doc["id"]} | {c["id"] for c in doc.get("children", [])}

    def cost(i):
        c = 0
        while i is not None and i not in keep:
            c, i = c + own[i], parent[i]
        return c

    for tag in sorted(need):
        want = 2 if tag.startswith("type:") else 1
        while sum(1 for i in keep if tag in feats[i]) < want:
            cands = [i for i in nodes if tag in feats[i] and i not in keep]
            if not cands:
                break
            i = min(cands, key=lambda i: (cost(i), i))
            while i is not None and i not in keep:
                keep.add(i)
                i = parent[i]

    def build(n):
        n = dict(n)
        if "children" in n:
            n["children"] = [build(c) for c in n["children"] if c["id"] in keep]
        return n

    return build(doc)


def prune_file_maps(body):
    """Drop `components`/`componentSets`/`styles` entries no kept node uses."""
    nodes, _ = index_nodes(body["document"])
    text = json.dumps(list(nodes.values()))
    # Sets go last: they're referenced from kept components' `componentSetId`.
    for k in ("components", "styles", "componentSets"):
        if isinstance(body.get(k), dict):
            body[k] = {i: v for i, v in body[k].items() if json.dumps(i) in text}
            text += json.dumps(body[k])


def all_features(doc):
    nodes, _ = index_nodes(doc)
    return set().union(*(node_features(n) for n in nodes.values()))


def dumps(v, indent=0, width=100):
    """JSON with short values kept on one line: readable diffs without the
    indentation of a fully pretty-printed (deeply nested) Figma document."""
    flat = json.dumps(v, ensure_ascii=False, separators=(", ", ": "))
    if len(flat) + indent <= width or not isinstance(v, (dict, list)) or not v:
        return flat
    pad = " " * (indent + 1)
    if isinstance(v, dict):
        items = [f"{pad}{json.dumps(k, ensure_ascii=False)}: {dumps(x, indent + 1, width)}" for k, x in v.items()]
        return "{\n" + ",\n".join(items) + "\n" + " " * indent + "}"
    items = [pad + dumps(x, indent + 1, width) for x in v]
    return "[\n" + ",\n".join(items) + "\n" + " " * indent + "]"


def filter_team_page(body, slug, real_keys):
    entries = body.get("meta", {}).get(slug, [])
    return [e for e in entries if e.get("file_key") in real_keys]


def record(token, folder, keys, team, redact):
    replacements = {k: fake_key(i) for i, k in enumerate(keys)}
    replacements[folder] = FAKE_FOLDER
    if team:
        replacements[team] = FAKE_TEAM
    scrub = Scrubber(replacements, redact)
    out = {}  # url path (with fake ids) -> (status, body)

    def rec(path, status, body):
        fake_path = path
        for real, fake in scrub.replacements:
            fake_path = fake_path.replace(real, fake)
        out[fake_path] = (status, body)
        print(f"  {status} {fake_path}", file=sys.stderr)

    status, listing = get(f"/v2/folders/{folder}/files", token)
    if status != 200:
        sys.exit(f"folder listing failed ({status}): {listing}")
    listed = {f["key"] for f in listing.get("files", [])}
    missing = [k for k in keys if k not in listed]
    if missing:
        sys.exit(f"REC_FILE_KEYS not in folder {folder}: {missing}")
    # Keep only the fixture files: other files' names in the folder never
    # land in the repo, and REC_FILE_KEYS order fixes the listing order.
    by_key = {f["key"]: f for f in listing.get("files", [])}
    listing["files"] = [by_key[k] for k in keys]
    rec(f"/v2/folders/{folder}/files", status, listing)

    # Feature coverage across the fixture files: a tag the first file already
    # covers isn't repeated in the second (node types always are, twice).
    source_features, covered = set(), set()
    for k in keys:
        for suffix in ("", "/meta", "/comments", "/variables/local"):
            path = f"/v1/files/{k}{suffix}"
            status, body = get(path, token)
            if suffix == "" and status == 200:
                feats = all_features(body["document"])
                source_features |= feats
                need = (feats - covered) | {t for t in feats if t.startswith("type:")}
                body["document"] = trim_document(body["document"], need)
                prune_file_maps(body)
                covered |= all_features(body["document"])
            rec(path, status, body)

    if team:
        for slug in ("components", "component_sets", "styles"):
            kept, after = [], None
            first = None
            while True:
                q = f"/v1/teams/{team}/{slug}?page_size=1000" + (f"&after={after}" if after else "")
                status, page = get(q, token)
                if status != 200:
                    sys.exit(f"{q} failed ({status}): {page}")
                first = first or page
                kept += filter_team_page(page, slug, set(keys))
                after = page.get("meta", {}).get("cursor", {}).get("after")
                if not after:
                    break
            # One page holding only fixture entries, no cursor: the e2e
            # suite builds multi-page responses itself when it needs them.
            body = dict(first)
            body["meta"] = {slug: kept}
            rec(f"/v1/teams/{team}/{slug}", 200, body)

    lost = source_features - covered
    if lost:
        sys.exit(f"trimming dropped features: {sorted(lost)}")
    print(f"  coverage: {len(covered)} feature tags kept: {', '.join(sorted(covered))}", file=sys.stderr)

    walked = {p: (st, scrub.walk(b)) for p, (st, b) in out.items()}
    rendered = {p: (st, scrub.text(b)) for p, (st, b) in walked.items()}
    leaks = {p: scrub.leaks(t) for p, (_, t) in rendered.items()}
    leaks = {p: l for p, l in leaks.items() if l}
    if leaks:
        sys.exit(f"scrub failed, identifiers survived: {leaks}")
    return rendered


def body_file(path):
    return path.lstrip("/") + ".json"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="diff against committed fixtures instead of writing")
    args = ap.parse_args()

    load_dotenv()
    try:
        token = os.environ["FIGMA_TOKEN"]
        folder = os.environ["REC_FOLDER_ID"]
        keys = [k.strip() for k in os.environ["REC_FILE_KEYS"].split(",") if k.strip()]
    except KeyError as e:
        sys.exit(f"missing env var {e}")
    team = os.environ.get("REC_TEAM_ID") or None
    redact = dict(
        pair.split("=", 1) for pair in os.environ.get("REC_REDACT", "").split(";") if "=" in pair
    )

    rendered = record(token, folder, keys, team, redact)
    routes = {p: {"status": st, "body": body_file(p)} for p, (st, _) in sorted(rendered.items())}
    routes_text = json.dumps(routes, indent=2) + "\n"

    if args.check:
        drift = False
        files = {"routes.json": routes_text}
        files.update({body_file(p): t for p, (_, t) in rendered.items()})
        for name, new in sorted(files.items()):
            f = OUT / name
            old = f.read_text() if f.exists() else ""
            if old != new:
                drift = True
                sys.stdout.writelines(
                    difflib.unified_diff(old.splitlines(True), new.splitlines(True), f"a/{name}", f"b/{name}")
                )
        sys.exit(1 if drift else 0)

    if OUT.exists():
        for f in sorted(OUT.rglob("*.json"), reverse=True):
            f.unlink()
    for p, (_, text) in rendered.items():
        f = OUT / body_file(p)
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(text)
    (OUT / "routes.json").write_text(routes_text)
    print(f"wrote {len(rendered)} fixtures to {OUT}", file=sys.stderr)


if __name__ == "__main__":
    main()
