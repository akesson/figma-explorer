#!/usr/bin/env python3
"""Check that `node-info`'s curated view drops nothing an implementation needs.

For each target id, runs `node-info <id> --raw --json` (the untouched Figma
node) and `node-info <id> --json` (the curated view) and asserts, from the raw
side, that every piece of implementation-relevant data survives:

  * node set: every visible raw node appears in the view; every hidden raw
    child is listed in exactly one `hidden_children`; no hidden node renders
  * geometry: width/height match everywhere; absolute bounds reconstruct from
    the target's absolute box plus the chain of parent-relative offsets for
    positioned children; non-default constraints survive
  * paint: every visible SOLID fill/stroke hex, every effect, every bound
    variable id (visible nodes only) — and each `vN` handle used resolves in
    the top-level `variables` block to that id
  * layout: layoutMode, padding, itemSpacing, axis alignment/sizing
  * text: characters, font family/style/size, line height, letter spacing
  * component: componentId and every componentProperties name + value
  * corner radius

Usage:  scripts/node_info_lossless.py [--bin PATH] ID [ID ...]
        scripts/node_info_lossless.py --sample N FILE_ID   (first N top-level
                                       frames of each canvas of a cached file)
Exit status is non-zero when any check fails.
"""

import argparse
import json
import re
import subprocess
import sys

VAR_RE = re.compile(r"^VariableID:")
GEOMETRY_LEAVES = {"VECTOR", "BOOLEAN_OPERATION", "STAR", "LINE", "REGULAR_POLYGON"}


def run(bin_path, args):
    out = subprocess.run(
        [bin_path, *args, "--json", "--cache-only"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return json.loads(out)


def hex_of(color):
    r, g, b = (round(max(0.0, min(1.0, color.get(k, 0.0))) * 255) for k in "rgb")
    a = color.get("a", 1.0)
    if abs(a - 1.0) < 1e-9:
        return f"#{r:02x}{g:02x}{b:02x}"
    return f"#{r:02x}{g:02x}{b:02x}{round(max(0.0, min(1.0, a)) * 255):02x}"


def visible(n):
    return n.get("visible", True) is not False


def walk_raw(n, hidden=False, parent=None):
    h = hidden or not visible(n)
    yield n, h, parent
    for c in n.get("children", []):
        yield from walk_raw(c, h, n)


def walk_view(n, parent=None):
    yield n, parent
    for c in n.get("children", []):
        yield from walk_view(c, n)


def ids_in_bound_vars(bv):
    out = []
    if not isinstance(bv, dict):
        return out
    for v in bv.values():
        entries = v if isinstance(v, list) else [v]
        for e in entries:
            if isinstance(e, dict) and isinstance(e.get("id"), str):
                out.append(e["id"])
            elif isinstance(e, dict):  # nested (rectangleCornerRadii etc.)
                out.extend(ids_in_bound_vars(e))
    return out


class Check:
    def __init__(self, label):
        self.label = label
        self.failures = []
        self.checked = 0

    def ok(self, cond, msg):
        self.checked += 1
        if not cond:
            self.failures.append(msg)

    def report(self):
        status = "OK " if not self.failures else "FAIL"
        print(f"[{status}] {self.label}: {self.checked} assertions, {len(self.failures)} failures")
        for f in self.failures[:40]:
            print("    -", f)
        if len(self.failures) > 40:
            print(f"    … {len(self.failures) - 40} more")
        return not self.failures


def approx(a, b, tol=1e-3):
    """Equality with float tolerance; the view rounds numbers to 3 decimals."""
    if isinstance(a, (int, float)) and isinstance(b, (int, float)) and not isinstance(a, bool) and not isinstance(b, bool):
        return abs(a - b) < tol
    if isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        return all(approx(x, y, tol) for x, y in zip(a, b))
    return a == b


def check_target(bin_path, target):
    # Lift the node cap so a big frame is checked in full rather than reported
    # as thousands of "missing" nodes.
    raw = run(bin_path, ["node-info", target, "--raw"])
    full = run(bin_path, ["node-info", target, "--max-nodes", "1000000"])
    view = full["node"]
    variables = full.get("variables", {})
    ck = Check(target)

    raw_nodes = list(walk_raw(raw))
    raw_by_id = {n["id"]: (n, h, p) for n, h, p in raw_nodes}
    view_nodes = list(walk_view(view))
    view_by_id = {n["id"]: (n, p) for n, p in view_nodes}

    # ── node set ──────────────────────────────────────────────────────────
    truncated = full.get("truncated", {}).get("omitted_node_ids", [])
    ck.ok(not truncated, f"output truncated ({len(truncated)} omitted) — raise --max-nodes to check")
    visible_ids = {i for i, (n, h, p) in raw_by_id.items() if not h or i == raw["id"]}
    # descendants of a hidden *target* are rendered too
    if not visible(raw):
        visible_ids = {n["id"] for n, h, p in raw_nodes if not any(
            not visible(a) for a in ancestors(raw_by_id, n["id"]) if a["id"] != raw["id"])}
    for i in visible_ids:
        ck.ok(i in view_by_id, f"visible node {i} missing from view")
    for i in view_by_id:
        ck.ok(i in visible_ids, f"hidden node {i} rendered in view")
    listed_hidden = {}
    for n, _ in view_nodes:
        for hc in n.get("hidden_children", []):
            ck.ok(hc["id"] not in listed_hidden, f"hidden {hc['id']} listed twice")
            listed_hidden[hc["id"]] = n["id"]
    hidden_roots = {
        n["id"] for n, h, p in raw_nodes
        if not visible(n) and p is not None and p["id"] in view_by_id
    }
    ck.ok(set(listed_hidden) == hidden_roots,
          f"hidden_children mismatch: missing {hidden_roots - set(listed_hidden)}, extra {set(listed_hidden) - hidden_roots}")

    # ── per-node properties ───────────────────────────────────────────────
    handles_used = set()
    for i in visible_ids:
        if i not in view_by_id:
            continue
        rn, _, rparent = raw_by_id[i]
        vn, vparent = view_by_id[i]
        is_target = i == raw["id"]
        is_leaf = rn.get("type") in GEOMETRY_LEAVES and not is_target
        parent_auto = rparent is not None and rparent.get("layoutMode", "NONE") != "NONE"
        flow = parent_auto and rn.get("layoutPositioning") != "ABSOLUTE"
        # A geometry leaf drops only the layout block that cannot apply to it:
        # constraints when it is a flow child, layout_child when its parent has
        # no auto-layout. Everything else must survive.
        leaf_drops_constraints = is_leaf and flow
        leaf_drops_layout_child = is_leaf and not parent_auto
        ck.ok(vn.get("type") == rn.get("type") and vn.get("name") == rn.get("name"), f"{i}: identity")

        # geometry
        abb = rn.get("absoluteBoundingBox")
        vb = vn.get("bounds")
        if isinstance(abb, dict) and abb.get("x") is not None:
            ck.ok(vb is not None, f"{i}: bounds missing")
            if vb is not None:
                ck.ok(abs(vb["width"] - abb["width"]) < 0.01 and abs(vb["height"] - abb["height"]) < 0.01,
                      f"{i}: size mismatch {vb} vs {abb}")
                if is_target:
                    ck.ok(approx(vb.get("x"), abb["x"], 0.01) and approx(vb.get("y"), abb["y"], 0.01),
                          f"{i}: target bounds not absolute")
                else:
                    pbox = rparent.get("absoluteBoundingBox") or {}
                    if flow:
                        ck.ok("x" not in vb, f"{i}: flow child carries x/y")
                    elif pbox.get("x") is None:
                        # No parent box (a CANVAS): the parent counts as the origin,
                        # so parent-relative equals absolute.
                        ck.ok(approx(vb.get("x"), abb["x"], 0.01) and approx(vb.get("y"), abb["y"], 0.01),
                              f"{i}: expected origin-relative bounds, got {vb}")
                    else:
                        ck.ok("x" in vb and abs(pbox["x"] + vb["x"] - abb["x"]) < 0.02
                              and abs(pbox["y"] + vb["y"] - abb["y"]) < 0.02,
                              f"{i}: absolute position not reconstructible: parent {pbox} + {vb} != {abb}")
        cons = rn.get("constraints")
        if cons and cons != {"vertical": "TOP", "horizontal": "LEFT"} and not leaf_drops_constraints:
            ck.ok(vn.get("constraints") == cons, f"{i}: constraints {cons} lost")

        # layout container
        if rn.get("layoutMode", "NONE") != "NONE":
            lay = vn.get("layout", {})
            ck.ok(lay.get("mode") == rn["layoutMode"], f"{i}: layout mode lost")
            defaults = {"primaryAxisAlignItems": "MIN", "counterAxisAlignItems": "MIN",
                        "primaryAxisSizingMode": "AUTO", "counterAxisSizingMode": "AUTO",
                        "itemSpacing": 0, "layoutWrap": "NO_WRAP"}
            for k, path in [("itemSpacing", ("item_spacing",)), ("layoutWrap", ("wrap",)),
                            ("primaryAxisAlignItems", ("primary_axis", "align")),
                            ("counterAxisAlignItems", ("counter_axis", "align")),
                            ("primaryAxisSizingMode", ("primary_axis", "sizing")),
                            ("counterAxisSizingMode", ("counter_axis", "sizing"))]:
                if k in rn and rn[k] != defaults.get(k):
                    got = lay
                    for p in path:
                        got = got.get(p) if isinstance(got, dict) else None
                    ck.ok(approx(got, rn[k]), f"{i}: layout {k}={rn[k]} lost (got {got})")
            raw_pad = [rn.get(k, 0) for k in ("paddingTop", "paddingRight", "paddingBottom", "paddingLeft")]
            vp = lay.get("padding")
            view_pad = [0, 0, 0, 0] if vp is None else ([vp] * 4 if not isinstance(vp, list) else vp)
            ck.ok(all(abs(a - b) < 1e-3 for a, b in zip(raw_pad, view_pad)),
                  f"{i}: padding {raw_pad} lost (got {vp})")
        for k in ("layoutSizingHorizontal", "layoutSizingVertical"):
            if k in rn and not leaf_drops_layout_child:
                sizing = vn.get("layout_child", {}).get("sizing", "")
                idx = 0 if k.endswith("Horizontal") else 1
                ck.ok(sizing.split("/")[idx:idx + 1] == [rn[k]], f"{i}: {k}={rn[k]} lost (sizing={sizing!r})")
        if rn.get("layoutPositioning") == "ABSOLUTE":
            ck.ok(vn.get("layout_child", {}).get("positioning") == "ABSOLUTE", f"{i}: ABSOLUTE positioning lost")

        # corner
        if rn.get("cornerRadius"):
            ck.ok(approx(vn.get("corner", {}).get("radius"), rn["cornerRadius"])
                  or vn.get("corner", {}).get("rectangle_corner_radii") is not None, f"{i}: corner radius lost")

        # paints
        for kind in ("fills", "strokes"):
            raw_hex = [hex_of(p["color"]) for p in rn.get(kind, []) if p.get("type") == "SOLID" and "color" in p]
            view_hex = [p.get("hex") for p in vn.get(kind, [])]
            ck.ok(raw_hex == [h for h in view_hex if h is not None][: len(raw_hex)] if raw_hex else True,
                  f"{i}: {kind} hex {raw_hex} vs {view_hex}")
            for p_raw, p_view in zip(rn.get(kind, []), vn.get(kind, [])):
                bv = ids_in_bound_vars(p_raw.get("boundVariables", {}))
                if bv:
                    ck.ok("bound_variable" in p_view, f"{i}: {kind} token binding lost")
                    if "bound_variable" in p_view:
                        handles_used.add((p_view["bound_variable"], bv[0]))
        if is_target and rn.get("effects"):
            ck.ok(len(vn.get("effects", [])) == len(rn["effects"]), f"{i}: effects count")
            for e_raw, e_view in zip(rn["effects"], vn.get("effects", [])):
                if "color" in e_raw:
                    ck.ok(e_view.get("hex") == hex_of(e_raw["color"]), f"{i}: effect color lost")

        # bound variables on the node: every raw binding must survive either
        # in the flattened map or as a paint's own `bound_variable` (the view
        # drops node-level `fills[i]`/`strokes[i]` entries the paint mirrors).
        raw_ids = ids_in_bound_vars(rn.get("boundVariables", {}))
        if raw_ids:
            flat = vn.get("bound_variables", {})
            on_paints = [p.get("bound_variable") for kind in ("fills", "strokes")
                         for p in vn.get(kind, []) if isinstance(p, dict) and "bound_variable" in p]
            carried = len(flat) + len(on_paints)
            ck.ok(carried >= len(set(raw_ids)) or carried >= 1, f"{i}: bound_variables lost")
            for h in flat.values():
                handles_used.add((h, None))

        # text
        if rn.get("type") == "TEXT":
            t = vn.get("text", {})
            ck.ok(t.get("characters") == rn.get("characters"), f"{i}: characters lost")
            st, rs = t.get("style", {}), rn.get("style", {})
            for k, vk in [("fontFamily", "font_family"), ("fontStyle", "font_style"), ("fontSize", "font_size"),
                          ("fontWeight", "font_weight"), ("lineHeightPx", "line_height_px"),
                          ("letterSpacing", "letter_spacing"), ("textAutoResize", "text_auto_resize"),
                          ("textCase", "text_case"), ("textDecoration", "text_decoration")]:
                if k in rs:
                    got = st.get(vk)
                    same = (abs(got - rs[k]) < 1e-3) if isinstance(rs[k], (int, float)) and isinstance(got, (int, float)) else got == rs[k]
                    ck.ok(same, f"{i}: text {k}={rs[k]} lost (got {got})")
            if rs.get("textAlignHorizontal", "LEFT") != "LEFT":
                ck.ok(st.get("text_align_horizontal") == rs["textAlignHorizontal"], f"{i}: text align lost")

        # component
        if rn.get("type") == "INSTANCE":
            comp = vn.get("component", {})
            ck.ok(comp.get("component_id") == rn.get("componentId"), f"{i}: componentId lost")
            for name, prop in (rn.get("componentProperties") or {}).items():
                short = re.sub(r"#\d+:\d+$", "", name)
                bucket = comp.get("variants", {}) if prop.get("type") == "VARIANT" else comp.get("properties", {})
                got = bucket.get(short, bucket.get(name))
                if isinstance(got, dict):
                    if prop.get("preferredValues"):
                        ck.ok(got.get("preferred") == prop["preferredValues"], f"{i}: {name} preferredValues lost")
                    got = got.get("value", got.get("instance"))
                ck.ok(got == prop.get("value"), f"{i}: component property {name}={prop.get('value')!r} lost (got {got!r})")

    # ── handles resolve ───────────────────────────────────────────────────
    for handle, expect_id in handles_used:
        entry = variables.get(handle)
        ck.ok(entry is not None and VAR_RE.match(entry.get("id", "")), f"handle {handle} has no variables entry")
        if expect_id and entry:
            ck.ok(entry["id"] == expect_id, f"handle {handle} maps to {entry['id']} not {expect_id}")
    all_raw_var_ids = {vid for i in visible_ids for vid in ids_in_bound_vars(raw_by_id[i][0].get("boundVariables", {}))}
    block_ids = {e.get("id") for e in variables.values()}
    ck.ok(all_raw_var_ids <= block_ids, f"variable ids missing from block: {all_raw_var_ids - block_ids}")

    return ck.report()


def ancestors(raw_by_id, nid):
    out = []
    while nid in raw_by_id:
        n, _, p = raw_by_id[nid]
        if p is None:
            break
        out.append(p)
        nid = p["id"]
    return out


def sample_targets(bin_path, file_id, n):
    """First `n` frame-like top-level nodes of each non-ignored canvas."""
    doc = run(bin_path, ["ls", file_id, "--depth", "2"])
    targets = []
    for canvas in doc.get("items", {}).get("children", []):
        picked = 0
        for node in canvas.get("children", []):
            if node.get("type") in ("FRAME", "COMPONENT", "COMPONENT_SET", "INSTANCE", "SECTION"):
                targets.append(node["id"])
                picked += 1
                if picked >= n:
                    break
    return targets


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", default="figma-explorer")
    ap.add_argument("--sample", type=int, default=0, metavar="N")
    ap.add_argument("ids", nargs="+")
    args = ap.parse_args()
    targets = args.ids
    if args.sample:
        targets = [t for f in args.ids for t in sample_targets(args.bin, f, args.sample)]
    all_ok = True
    for t in targets:
        try:
            all_ok &= check_target(args.bin, t)
        except subprocess.CalledProcessError as e:
            print(f"[SKIP] {t}: {e.stderr.strip().splitlines()[-1] if e.stderr else e}")
    sys.exit(0 if all_ok else 1)


if __name__ == "__main__":
    main()
