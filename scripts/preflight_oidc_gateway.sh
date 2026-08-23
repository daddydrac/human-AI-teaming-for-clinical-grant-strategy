#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

[[ -f .env ]] || { echo "ERROR: .env is required." >&2; exit 2; }
set -a
source .env
[[ -f .runtime.env ]] && source .runtime.env
set +a

required=(OIDC_ISSUER_URL OIDC_CLIENT_ID OIDC_ALLOWED_EMAIL_DOMAINS OIDC_PUBLIC_HOST OIDC_ORGANIZATION_ID OIDC_CLIENT_SECRET_FILE OIDC_COOKIE_SECRET_FILE OIDC_GATEWAY_SHARED_SECRET_FILE OIDC_TLS_CERT_FILE OIDC_TLS_KEY_FILE)
for name in "${required[@]}"; do
  [[ -n "${!name:-}" ]] || { echo "ERROR: $name is required for the OIDC gateway." >&2; exit 3; }
done

python3 - <<'PY'
import json, os, re, ssl, sys
from pathlib import Path
from urllib.parse import urlparse
from urllib.request import Request, urlopen

issuer=os.environ['OIDC_ISSUER_URL'].rstrip('/')
parsed=urlparse(issuer)
if parsed.scheme!='https' or not parsed.netloc or parsed.query or parsed.fragment:
    raise SystemExit('ERROR: OIDC_ISSUER_URL must be an HTTPS issuer URL without query or fragment.')
host=os.environ['OIDC_PUBLIC_HOST']
if not re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?',host) or '..' in host:
    raise SystemExit('ERROR: OIDC_PUBLIC_HOST must be a DNS hostname without a scheme, path, or port.')
organization=os.environ['OIDC_ORGANIZATION_ID']
if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._:-]{1,127}',organization):
    raise SystemExit('ERROR: OIDC_ORGANIZATION_ID must be a stable 2-128 character identifier.')
claim=os.getenv('OIDC_USER_ID_CLAIM','sub').strip()
if not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_.:-]{0,127}',claim):
    raise SystemExit('ERROR: OIDC_USER_ID_CLAIM is not a valid claim name.')
if claim in {'email','preferred_username','name'}:
    raise SystemExit('ERROR: OIDC_USER_ID_CLAIM must be immutable; use sub or an institution-managed immutable subject claim.')
domains=[item.strip().lower() for item in os.environ['OIDC_ALLOWED_EMAIL_DOMAINS'].split(',') if item.strip()]
if not domains or '*' in domains or any(not re.fullmatch(r'[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?',item) for item in domains):
    raise SystemExit('ERROR: OIDC_ALLOWED_EMAIL_DOMAINS must contain explicit comma-separated DNS domains; wildcard access is prohibited.')
for name in ('OIDC_CLIENT_SECRET_FILE','OIDC_TLS_CERT_FILE','OIDC_TLS_KEY_FILE'):
    path=Path(os.environ[name])
    if not path.is_file() or path.is_symlink():
        raise SystemExit(f'ERROR: {name} must reference a regular, non-symlink file: {path}')
client_secret=Path(os.environ['OIDC_CLIENT_SECRET_FILE']).read_bytes()
if not client_secret or client_secret.endswith((b'\n',b'\r')):
    raise SystemExit('ERROR: OIDC client secret must be non-empty and contain no trailing newline.')

certificate=ssl._ssl._test_decode_cert(os.environ['OIDC_TLS_CERT_FILE'])
dns_names=[value.lower().rstrip('.') for kind,value in certificate.get('subjectAltName',()) if kind=='DNS']
if not dns_names:
    dns_names=[value.lower().rstrip('.') for rdn in certificate.get('subject',()) for key,value in rdn if key=='commonName']
def dns_matches(pattern,hostname):
    pattern_labels=pattern.split('.')
    host_labels=hostname.lower().rstrip('.').split('.')
    if len(pattern_labels)!=len(host_labels):return False
    if '*' not in pattern:return pattern_labels==host_labels
    return pattern_labels[0]=='*' and bool(host_labels[0]) and pattern_labels[1:]==host_labels[1:]
if not any(dns_matches(pattern,host) for pattern in dns_names):
    raise SystemExit('ERROR: TLS certificate is not valid for OIDC_PUBLIC_HOST.')

discovery_url=issuer+'/.well-known/openid-configuration'
request=Request(discovery_url,headers={'Accept':'application/json','User-Agent':'Grantspace-OIDC-Preflight/1.0'})
try:
    with urlopen(request,timeout=15,context=ssl.create_default_context()) as response:
        if response.status != 200:
            raise ValueError(f'HTTP {response.status}')
        document=json.load(response)
except Exception as error:
    raise SystemExit(f'ERROR: OIDC discovery failed for {discovery_url}: {error}')
if document.get('issuer','').rstrip('/') != issuer:
    raise SystemExit('ERROR: OIDC discovery issuer does not exactly match OIDC_ISSUER_URL.')
for field in ('authorization_endpoint','token_endpoint','jwks_uri'):
    value=document.get(field)
    if not isinstance(value,str) or urlparse(value).scheme!='https':
        raise SystemExit(f'ERROR: OIDC discovery document lacks a valid HTTPS {field}.')
if 'S256' not in document.get('code_challenge_methods_supported',[]):
    raise SystemExit('ERROR: the OIDC provider must advertise PKCE S256 support.')
print(f'OIDC discovery verified: issuer={issuer}')
PY

mkdir -p "$(dirname "$OIDC_COOKIE_SECRET_FILE")"
if [[ ! -e "$OIDC_COOKIE_SECRET_FILE" ]]; then
  umask 077
  openssl rand 32 > "$OIDC_COOKIE_SECRET_FILE"
  echo "Generated a 32-byte OAuth session-cookie secret at $OIDC_COOKIE_SECRET_FILE"
fi
[[ -f "$OIDC_COOKIE_SECRET_FILE" && ! -L "$OIDC_COOKIE_SECRET_FILE" ]] || { echo "ERROR: OIDC_COOKIE_SECRET_FILE must be a regular, non-symlink file." >&2; exit 4; }
COOKIE_BYTES="$(wc -c < "$OIDC_COOKIE_SECRET_FILE" | tr -d ' ')"
[[ "$COOKIE_BYTES" == 16 || "$COOKIE_BYTES" == 24 || "$COOKIE_BYTES" == 32 ]] || { echo "ERROR: OIDC cookie secret must contain exactly 16, 24, or 32 raw bytes." >&2; exit 4; }

mkdir -p "$(dirname "$OIDC_GATEWAY_SHARED_SECRET_FILE")"
if [[ ! -e "$OIDC_GATEWAY_SHARED_SECRET_FILE" ]]; then
  umask 077
  printf '%s' "$(openssl rand -hex 32)" > "$OIDC_GATEWAY_SHARED_SECRET_FILE"
  echo "Generated a 256-bit gateway proof secret at $OIDC_GATEWAY_SHARED_SECRET_FILE"
fi
[[ -f "$OIDC_GATEWAY_SHARED_SECRET_FILE" && ! -L "$OIDC_GATEWAY_SHARED_SECRET_FILE" ]] || { echo "ERROR: OIDC_GATEWAY_SHARED_SECRET_FILE must be a regular, non-symlink file." >&2; exit 4; }
GATEWAY_SECRET="$(LC_ALL=C tr -d '0123456789abcdefABCDEF' < "$OIDC_GATEWAY_SHARED_SECRET_FILE")"
GATEWAY_BYTES="$(wc -c < "$OIDC_GATEWAY_SHARED_SECRET_FILE" | tr -d ' ')"
[[ "$GATEWAY_BYTES" == 64 && -z "$GATEWAY_SECRET" ]] || { echo "ERROR: gateway proof secret must contain exactly 64 hexadecimal characters." >&2; exit 4; }

for private_file in "$OIDC_CLIENT_SECRET_FILE" "$OIDC_COOKIE_SECRET_FILE" "$OIDC_GATEWAY_SHARED_SECRET_FILE" "$OIDC_TLS_KEY_FILE"; do
  chmod go-rwx "$private_file"
done
openssl x509 -in "$OIDC_TLS_CERT_FILE" -noout -checkend 86400 >/dev/null || { echo "ERROR: TLS certificate is invalid or expires within 24 hours." >&2; exit 5; }
CERT_PUBLIC="$(openssl x509 -in "$OIDC_TLS_CERT_FILE" -pubkey -noout | openssl pkey -pubin -outform DER 2>/dev/null | openssl dgst -sha256)"
KEY_PUBLIC="$(openssl pkey -in "$OIDC_TLS_KEY_FILE" -pubout -outform DER 2>/dev/null | openssl dgst -sha256)"
[[ -n "$CERT_PUBLIC" && "$CERT_PUBLIC" == "$KEY_PUBLIC" ]] || { echo "ERROR: TLS certificate and private key do not match." >&2; exit 5; }

docker compose -f docker-compose.yml -f docker-compose.oidc.yml config >/dev/null
echo "OIDC gateway preflight passed for https://${OIDC_PUBLIC_HOST}:${OIDC_HTTPS_PORT:-8443}"
