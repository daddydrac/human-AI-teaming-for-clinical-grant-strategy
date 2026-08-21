#!/usr/bin/env bash
set -euo pipefail
ARTIFACT="${1:-}"
[[ -f "$ARTIFACT" ]] || { echo "Usage: $0 release.zip" >&2; exit 2; }
SHA_FILE="$ARTIFACT.sha256"
shasum -a 256 "$ARTIFACT" > "$SHA_FILE"
echo "Checksum written: $SHA_FILE"
if [[ -n "${MINISIGN_SECRET_KEY:-}" ]] && command -v minisign >/dev/null 2>&1; then
  minisign -Sm "$ARTIFACT" -s "$MINISIGN_SECRET_KEY"
  echo "Minisign signature created."
elif [[ -n "${GPG_SIGNING_KEY:-}" ]] && command -v gpg >/dev/null 2>&1; then
  gpg --batch --yes --local-user "$GPG_SIGNING_KEY" --armor --detach-sign "$ARTIFACT"
  echo "GPG signature created."
elif [[ "${REQUIRE_RELEASE_SIGNATURE:-false}" == "true" ]]; then
  echo "ERROR: release signature required but no supported signing key/tool is configured." >&2
  exit 3
else
  echo "No signing key configured; checksum created, cryptographic signing skipped."
fi
