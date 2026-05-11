#!/usr/bin/env bash
# Refresh openapi/openapi.yaml from upstream and update openapi/SPEC_VERSION.
set -euo pipefail
cd "$(dirname "$0")/.."

UPSTREAM_REPO="figma/rest-api-spec"
SPEC_PATH="openapi/openapi.yaml"

SHA=$(curl -fsSL "https://api.github.com/repos/${UPSTREAM_REPO}/commits/main" \
    | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['sha'])")
DATE=$(curl -fsSL "https://api.github.com/repos/${UPSTREAM_REPO}/commits/${SHA}" \
    | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['commit']['committer']['date'])")

curl -fsSL -o "${SPEC_PATH}" \
    "https://raw.githubusercontent.com/${UPSTREAM_REPO}/${SHA}/openapi/openapi.yaml"

cat > openapi/SPEC_VERSION <<EOF
commit: ${SHA}
date:   ${DATE}
source: https://github.com/${UPSTREAM_REPO}/blob/${SHA}/openapi/openapi.yaml
EOF

echo "Updated ${SPEC_PATH} to ${SHA} (${DATE})."
