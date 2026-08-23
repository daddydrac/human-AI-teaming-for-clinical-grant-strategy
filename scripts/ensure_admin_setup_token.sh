#!/usr/bin/env bash
set -euo pipefail

ENV_FILE="${1:-.env}"
[[ -f "$ENV_FILE" ]] || { echo "ERROR: environment file does not exist: $ENV_FILE" >&2; exit 2; }
command -v openssl >/dev/null 2>&1 || { echo "ERROR: openssl is required to generate the administrator setup token." >&2; exit 3; }

EXISTING_TOKEN="$(awk -F= '$1=="INITIAL_ADMIN_SETUP_TOKEN" {sub(/^[^=]*=/,""); print; exit}' "$ENV_FILE")"
if [[ -n "$EXISTING_TOKEN" ]]; then
  echo "Initial administrator setup token is already configured in $ENV_FILE."
  echo "Copy only this 64-character token into the GUI:"
  echo "$EXISTING_TOKEN"
  exit 0
fi

TOKEN="$(openssl rand -hex 32)"
TEMP_FILE="$(mktemp "${ENV_FILE}.tmp.XXXXXX")"
awk -v token="$TOKEN" '
  BEGIN { replaced=0 }
  /^INITIAL_ADMIN_SETUP_TOKEN=/ {
    if (!replaced) print "INITIAL_ADMIN_SETUP_TOKEN=" token
    replaced=1
    next
  }
  { print }
  END { if (!replaced) print "INITIAL_ADMIN_SETUP_TOKEN=" token }
' "$ENV_FILE" > "$TEMP_FILE"
chmod 600 "$TEMP_FILE"
mv "$TEMP_FILE" "$ENV_FILE"

echo "Generated the one-time initial administrator setup token in $ENV_FILE."
echo "Copy only this 64-character token into the GUI:"
echo "$TOKEN"
echo "Paste this value into the first-start GUI at http://localhost:7860/setup"
