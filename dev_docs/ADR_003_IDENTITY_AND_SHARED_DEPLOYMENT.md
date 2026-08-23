# ADR 003 — Identity Boundary and Shared Deployment

Status: accepted  
Date: 2026-08-23

## Decision

Grantspace supports three explicit authentication profiles:

1. `internal_accounts` is the default bounded deployment. Exactly one initial
   system administrator bootstraps the installation. That administrator creates
   subsequent accounts, and every new account must change its temporary password.
2. `trusted_headers` is the shared enterprise contract. The API trusts identity
   headers only because the UI and API have no public host ports in this profile.
   A TLS authentication gateway overwrites those headers after authenticating the
   browser.
3. `local_single_user` exists only for local development and acceptance testing.

The reference enterprise deployment uses a pinned OAuth2 Proxy to perform OIDC
Authorization Code + PKCE authentication and a pinned Nginx gateway to terminate
TLS. The OIDC immutable subject claim is mapped to
`X-Grantspace-User-Id`; email and preferred username remain separate claims. A
deployment-wide stable organization ID is mapped to
`X-Grantspace-Organization-Id`.

Institutional SAML gateways are supported through the same trusted-header
contract. They must authenticate the request, map an immutable NameID or directory
object ID—not an email address—to `X-Grantspace-User-Id`, overwrite all four
Grantspace identity headers, inject the deployment's private gateway proof, and
be the only service able to reach the UI.

## Security invariants

- Core, UI, renderer, ingestion, and embedding services are not published to the
  host in the enterprise override. Only the TLS gateway is published.
- The gateway overwrites, rather than appends, browser-provided identity headers.
- OAuth2 Proxy injects a deployment-generated 256-bit proof after authentication;
  the UI and API verify it before accepting trusted identity headers.
- The stable user claim cannot be `email`, `preferred_username`, or `name`.
- Wildcard email-domain admission is rejected.
- OIDC issuer metadata must match the configured HTTPS issuer exactly and expose
  HTTPS authorization, token, and JWKS endpoints.
- PKCE S256 and nonce validation are mandatory.
- Client, cookie, gateway-proof, and TLS private-key material is excluded from
  source control and mounted read-only from deployment-owned files.
- The TLS certificate and key must match, and the certificate must remain valid
  for at least 24 hours at startup.

## Consequences

The current shared-server profile intentionally represents one organization per
deployment. Cross-organization tenancy requires a separately reviewed claim-to-
organization mapping and tenant-isolation test suite; it must not be approximated
from an email domain. Changing the immutable user claim or issuer is an identity
migration and requires an audited membership-mapping plan.
