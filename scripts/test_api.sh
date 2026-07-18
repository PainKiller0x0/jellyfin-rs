#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENV="${JELLYFIN_RS_API_TEST_VENV:-/tmp/jellyfin-rs-api-venv}"
BASE_URL="${JELLYFIN_RS_API_BASE_URL:-http://127.0.0.1:8096}"
USERNAME="${JELLYFIN_RS_USER:-admin}"
PASSWORD="${JELLYFIN_RS_PASSWORD:-123456}"

if [[ ! -x "$VENV/bin/python" ]]; then
  uv venv "$VENV"
  uv pip install --python "$VENV/bin/python" httpx
fi

"$VENV/bin/python" "$ROOT/scripts/tsukimi_jellyfin_api_smoke.py" \
  --base-url "$BASE_URL" \
  --username "$USERNAME" \
  --password "$PASSWORD"

exec "$VENV/bin/python" "$ROOT/scripts/jellyfin_openapi_compat.py" \
  --base-url "$BASE_URL" \
  --username "$USERNAME" \
  --password "$PASSWORD" \
  --spec "$ROOT/docs/jellyfin-openapi-stable.json" \
  --routes-source "$ROOT/src/jellyfin/routes.rs"
