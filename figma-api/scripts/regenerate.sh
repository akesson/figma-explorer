#!/usr/bin/env bash
# Regenerate src/ from openapi/openapi.yaml using openapi-generator-cli.
# Requires: node/npm (for the openapi-generator-cli wrapper) and a JDK (Java 11+).
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v npx >/dev/null 2>&1; then
    echo "error: npx not found. Install Node.js (https://nodejs.org)." >&2
    exit 1
fi

# Discover a usable JDK. Prefer an explicit JAVA_HOME; otherwise probe common
# macOS / Homebrew install locations. openapi-generator needs Java 11+.
if [ -z "${JAVA_HOME:-}" ] || [ ! -x "${JAVA_HOME}/bin/java" ]; then
    for candidate in \
        /opt/homebrew/opt/openjdk \
        /opt/homebrew/opt/openjdk@21 \
        /opt/homebrew/opt/openjdk@17 \
        /usr/local/opt/openjdk \
        /usr/local/opt/openjdk@21 \
        /usr/local/opt/openjdk@17; do
        if [ -x "${candidate}/bin/java" ]; then
            export JAVA_HOME="${candidate}"
            break
        fi
    done
fi

if [ -n "${JAVA_HOME:-}" ] && [ -x "${JAVA_HOME}/bin/java" ]; then
    export PATH="${JAVA_HOME}/bin:${PATH}"
fi

if ! java -version >/dev/null 2>&1; then
    cat >&2 <<'EOF'
error: a working Java runtime (JDK 11+) is required by openapi-generator.
       set JAVA_HOME to point at a JDK, or install one, e.g.:
         brew install openjdk@21
EOF
    exit 1
fi

# Wipe previously-generated trees so removed schemas/apis don't linger.
rm -rf src/apis src/models docs

npx --yes @openapitools/openapi-generator-cli generate \
    -i openapi/openapi.yaml \
    -c openapi/config.yaml \
    -g rust \
    -o .

python3 scripts/postprocess.py

# Contingency: apply hand-maintained patches for issues postprocess.py can't handle.
shopt -s nullglob
for p in openapi/patches/*.patch; do
    echo "applying $p"
    git apply --whitespace=nowarn "$p"
done

cargo fmt -p figma-api
cargo build -p figma-api

echo "figma-api regenerated and built."
