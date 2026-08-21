#!/usr/bin/env bash
set -euo pipefail
ROOT="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$ROOT"
fail=0
bad_private=$(find . -type f \( -name '.env' -o -name '.runtime.env' -o -name '*.sqlite' -o -name '*.db' -o -name '*.pyc' \) -print || true)
if [[ -n "$bad_private" ]]; then
  echo "Private/runtime artifacts found in release tree:" >&2
  echo "$bad_private" >&2
  fail=1
fi
bad_dirs=$(find . -type d \( -name '__pycache__' -o -name '.pytest_cache' -o -name 'target' -o -name '.venv' \) -print || true)
if [[ -n "$bad_dirs" ]]; then
  echo "Build/cache directories found in release tree:" >&2
  echo "$bad_dirs" >&2
  fail=1
fi
if grep -RInE --exclude-dir=.git --exclude-dir=releases --exclude-dir=backups --exclude='*.zip' \
  '(sk-ant-[A-Za-z0-9_-]{12,}|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{20,}|BSA[A-Za-z0-9]{20,})' .; then
  echo "Potential embedded credential found." >&2
  fail=1
fi
grep -q '^\.env$' .dockerignore || { echo ".env missing from .dockerignore" >&2; fail=1; }
grep -q '^\.runtime.env$' .dockerignore || { echo ".runtime.env missing from .dockerignore" >&2; fail=1; }
grep -q '127.0.0.1:7860:7860' docker-compose.yml || { echo "UI port is not loopback-only" >&2; fail=1; }
grep -q '127.0.0.1:8080:8080' docker-compose.yml || { echo "Core port is not loopback-only" >&2; fail=1; }
grep -q 'no-new-privileges:true' docker-compose.yml || { echo "Docker no-new-privileges hardening missing" >&2; fail=1; }
grep -q 'cap_drop:' docker-compose.yml || { echo "Docker capability drop hardening missing" >&2; fail=1; }
[[ "$fail" == 0 ]] || exit 10
echo "Security scan passed: $ROOT"
