# Security

## Supported versions

Fixes go into the latest released version on PyPI. There are no long-term support branches.

| Version | Supported |
|---|---|
| 2.x, latest release | Yes |
| Older 2.x releases | No — upgrade to the latest |
| 1.x | No — upgrade to 2.x; the changelog's *Upgrading from 1.x* lists what changes |

## Reporting a vulnerability

Please report privately, not in a public issue.

Use [GitHub's private vulnerability reporting](https://github.com/sercanatalik/cereyan/security/advisories/new)
for this repository. Include what you found, how to reproduce it, and what an attacker could
do with it. You can expect an acknowledgement within a week.

## Scope worth knowing before you report

Cereyan is local-first by design, and some of what might look like a vulnerability is a
documented choice:

- The server binds `127.0.0.1:4200` by default and has no authentication there unless an API
  token is set: any process on the machine can use it. Bound to any other address it requires
  a token, generating one into the home when none is configured. Serving on the network
  without a token takes `allow_unauthenticated`, which the server warns about at start and the
  UI shows a banner for; a server that was opted out is a deployment decision, not a defect.
  The check is on the bound address, so a loopback server behind a reverse proxy is not caught
  and needs a token of its own — see
  [Secure the server](https://sercanatalik.github.io/cereyan/guides/secure-the-server/).
- Flows and tasks are arbitrary Python that you supply, executed in engine child processes.
  Anyone who can register a flow can run code as the serving user, by design.
- The store is a local SQLite file whose permissions are the operating system's to enforce.
  Variables marked secret are encrypted at rest; nothing else is.

Reports about the token check, the Unix socket permissions, the secret encryption, the
custom-route surface, or the MCP tool surface are in scope and welcome.
