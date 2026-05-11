#!/usr/bin/env python3
"""Post-process openapi-generator output to fix known Rust compile issues.

Currently handles one issue: when a Figma `oneOf` is a union of multiple
inline `type: array` schemas, openapi-generator names every variant `Array`,
producing `pub enum Foo { Array(Vec<A>), Array(Vec<B>) }` which won't
compile. Because the enums are `#[serde(untagged)]`, variant names don't
affect the wire format — so we suffix duplicates with 2, 3, ... to make
them unique.

Run from anywhere; the script locates the crate via its own path.
"""

import re
from pathlib import Path

CRATE_ROOT = Path(__file__).resolve().parent.parent
MODELS_DIR = CRATE_ROOT / "src" / "models"

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


def main() -> int:
    if not MODELS_DIR.is_dir():
        print(f"postprocess: nothing to do, {MODELS_DIR} does not exist")
        return 0
    changed = 0
    for path in sorted(MODELS_DIR.glob("*.rs")):
        original = path.read_text()
        fixed = dedupe_enum_variants(original)
        if fixed != original:
            path.write_text(fixed)
            changed += 1
            print(f"  fixed {path.relative_to(CRATE_ROOT)}")
    print(f"postprocess: rewrote {changed} file(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
