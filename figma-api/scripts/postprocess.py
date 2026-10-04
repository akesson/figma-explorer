#!/usr/bin/env python3
"""Post-process openapi-generator output to fix known Rust compile / runtime issues.

Currently handles four issues:

1. Duplicate `Array` enum variants. When a Figma `oneOf` is a union of
   multiple inline `type: array` schemas, openapi-generator names every
   variant `Array`, producing `pub enum Foo { Array(Vec<A>), Array(Vec<B>) }`
   which won't compile. Because the enums are `#[serde(untagged)]`, variant
   names don't affect the wire format — so we suffix duplicates with 2, 3, ...
   to make them unique.

2. Duplicate `X-Figma-Token` header emission. For endpoints that declare both
   PAT and OAuth security in the spec, openapi-generator emits the
   `if let Some(ref apikey) = configuration.api_key { ... }` block twice,
   so reqwest sends two identical `X-Figma-Token` headers. Figma rejects
   that combo with `403 Invalid token` on every non-trivial endpoint. We
   collapse the doubled block back to a single one.

3. Singleton-enum sentinel fields. The spec models `status: 200` and
   `error: false` as single-value enums; openapi-generator emits them as
   Rust enums whose only variant serializes as the *string* "200" / "false".
   The wire reality is integer 200 and boolean false, so every successful
   response fails to deserialize with `expected value at line 1 column 10`.
   We add `#[serde(skip)]` so the field is excluded from (de)serialization
   and falls back to its `Default` value — losing the sentinel from the
   output, which is the right answer since it carries no information.

4. Shadowed untagged-enum variants. `#[serde(untagged)]` takes the *first*
   variant that deserializes, and serde ignores unknown fields — so when a
   later variant's struct is an earlier one's plus extra required fields
   (`Region` = `Vector` + `region_width`/`region_height`; `Rgba` = `Rgb` +
   `a`), input meant for the later variant matches the earlier one and the
   extra fields are silently dropped. We move such a variant ahead of the
   one shadowing it. Inputs lacking the extra required fields fail it and
   fall through, so the earlier variant still gets its own input. Pairs
   already told apart by a shared field (e.g. `type` enums with disjoint
   values) are left alone.

Run from anywhere; the script locates the crate via its own path.
"""

import re
from pathlib import Path

CRATE_ROOT = Path(__file__).resolve().parent.parent
MODELS_DIR = CRATE_ROOT / "src" / "models"
APIS_DIR = CRATE_ROOT / "src" / "apis"

ENUM_DECL = re.compile(r"^\s*pub enum [A-Za-z0-9_]+\s*\{")
VARIANT_LINE = re.compile(r"^(\s*)([A-Z][A-Za-z0-9_]*)(\(.*\),?\s*)$")


def dedupe_enum_variants(text: str) -> str:
    out = []
    depth = 0
    seen: dict[str, int] = {}
    for line in text.splitlines(keepends=True):
        if depth == 0 and ENUM_DECL.match(line):
            depth = 1
            seen = {}
            out.append(line)
            continue
        if depth > 0:
            if "{" in line:
                depth += line.count("{")
            if "}" in line:
                depth -= line.count("}")
                if depth == 0:
                    seen = {}
                    out.append(line)
                    continue
            m = VARIANT_LINE.match(line)
            if m and depth == 1:
                indent, name, rest = m.groups()
                count = seen.get(name, 0) + 1
                seen[name] = count
                if count > 1:
                    line = f"{indent}{name}{count}{rest}\n" if not rest.endswith("\n") else f"{indent}{name}{count}{rest}"
        out.append(line)
    return "".join(out)


APIKEY_BLOCK = (
    '    if let Some(ref apikey) = configuration.api_key {\n'
    '        let key = apikey.key.clone();\n'
    '        let value = match apikey.prefix {\n'
    '            Some(ref prefix) => format!("{} {}", prefix, key),\n'
    '            None => key,\n'
    '        };\n'
    '        req_builder = req_builder.header("X-Figma-Token", value);\n'
    '    };\n'
)


def dedupe_apikey_blocks(text: str) -> str:
    doubled = APIKEY_BLOCK + APIKEY_BLOCK
    while doubled in text:
        text = text.replace(doubled, APIKEY_BLOCK)
    return text


SENTINEL_FIELDS = (
    ('    #[serde(rename = "status")]\n    pub status: Status,\n',
     '    #[serde(skip)]\n    pub status: Status,\n'),
    ('    #[serde(rename = "error")]\n    pub error: Error,\n',
     '    #[serde(skip)]\n    pub error: Error,\n'),
)


def skip_sentinel_enums(text: str) -> str:
    for old, new in SENTINEL_FIELDS:
        text = text.replace(old, new)
    return text


# Considered, NOT applied: Figma's beta spec drifts from the live
# API in multiple ways — some fields marked required come back null
# (`version` on `GET /v1/files/{key}`), and some endpoint responses are
# wrapped in an envelope the spec doesn't describe (`GET .../meta` returns
# `{"file": {...}}`). A blanket container-level `#[serde(default)]` was
# trialled here; it makes the small-drift cases work but silently turns
# big-drift cases into empty structs (no error, no data). Loud failures are
# more honest than silent corruption, so the blanket fix is left out. Per-
# endpoint patches (lifting envelopes, making specific fields Option) go in
# openapi/patches/ when they come up.


UNTAGGED_ENUM = re.compile(r"(#\[serde\(untagged\)\]\s*pub enum \w+\s*\{\n)(.*?)(\n\})", re.S)
STRUCT_VARIANT = re.compile(r"^\s*(\w+)\((?:Box<)?models::(\w+)>?\),?\s*$")
FIELD = re.compile(r"#\[serde\((.*?)\)\]\s*pub (?:r#)?\w+: ([^,\n]+),", re.S)


def model_file(type_name: str) -> Path:
    return MODELS_DIR / (re.sub(r"(?<!^)(?=[A-Z])", "_", type_name).lower() + ".rs")


def struct_fields(type_name: str):
    """`{wire_name: (rust_type, required, enum_values_or_None)}` for a model
    struct, or None when it isn't a plain struct in its own file."""
    path = model_file(type_name)
    if not path.exists():
        return None
    src = path.read_text()
    body = re.search(r"pub struct " + type_name + r"\s*\{(.*?)\n\}", src, re.S)
    if not body:
        return None
    out = {}
    for attr, ty in FIELD.findall(body.group(1)):
        rename = re.search(r'rename = "([^"]+)"', attr)
        if not rename:
            continue
        required = not ty.startswith("Option<") and "default" not in attr
        base = ty.replace("Option<", "").rstrip(">").strip()
        local = re.search(r"pub enum " + re.escape(base) + r"\s*\{(.*?)\n\}", src, re.S)
        values = set(re.findall(r'rename = "([^"]+)"', local.group(1))) if local else None
        out[rename.group(1)] = (base, required, values)
    return out


def shadows(a, b) -> bool:
    """Whether variant struct `a` (listed first) captures input meant for
    `b`: everything `a` requires is present in `b`, `b` requires something
    `a` lacks, and no shared required field tells them apart."""
    if a is None or b is None:
        return False
    req_a = {k for k, (_, r, _) in a.items() if r}
    if not req_a <= set(b) or not {k for k, (_, r, _) in b.items() if r} - set(a):
        return False
    for k in set(a) & set(b):
        (ta, ra, va), (tb, _, vb) = a[k], b[k]
        if va is not None and vb is not None and not (va & vb):
            return False  # e.g. `type`/`blurType` enums with disjoint values
        if ra and ((va is None) != (vb is None) or (va is None and ta != tb)):
            return False  # different Rust types; let serde decide
    return True


def reorder_shadowed_variants(text: str) -> str:
    def fix(m: re.Match) -> str:
        lines = m.group(2).split("\n")
        # Non-struct variants (`Boolean(bool)`, …) get no fields and never
        # shadow or get shadowed; they only move if a struct jumps them.
        parsed = [STRUCT_VARIANT.match(l) for l in lines]
        fields = [struct_fields(p.group(2)) if p else None for p in parsed]
        order = list(range(len(lines)))
        moved = True
        while moved:
            moved = False
            for i in range(len(order)):
                for j in range(i + 1, len(order)):
                    if shadows(fields[order[i]], fields[order[j]]):
                        order.insert(i, order.pop(j))
                        moved = True
                        break
                if moved:
                    break
        return m.group(1) + "\n".join(lines[k] for k in order) + m.group(3)

    return UNTAGGED_ENUM.sub(fix, text)


def main() -> int:
    changed = 0
    if MODELS_DIR.is_dir():
        for path in sorted(MODELS_DIR.glob("*.rs")):
            original = path.read_text()
            fixed = reorder_shadowed_variants(skip_sentinel_enums(dedupe_enum_variants(original)))
            if fixed != original:
                path.write_text(fixed)
                changed += 1
                print(f"  fixed (models)  {path.relative_to(CRATE_ROOT)}")
    else:
        print(f"postprocess: {MODELS_DIR} does not exist, skipping models pass")

    if APIS_DIR.is_dir():
        for path in sorted(APIS_DIR.glob("*_api.rs")):
            original = path.read_text()
            fixed = dedupe_apikey_blocks(original)
            if fixed != original:
                path.write_text(fixed)
                changed += 1
                print(f"  fixed (headers) {path.relative_to(CRATE_ROOT)}")
    else:
        print(f"postprocess: {APIS_DIR} does not exist, skipping header pass")

    print(f"postprocess: rewrote {changed} file(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
