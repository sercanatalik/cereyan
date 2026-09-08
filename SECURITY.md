# Security

## Supported versions

Fixes go into the latest released version on PyPI. There are no long-term support branches.

| Version | Supported |
|---|---|
| Latest release | Yes |
| Anything older | No — upgrade first |

## Reporting a vulnerability

Please report privately, not in a public issue.

Use [GitHub's private vulnerability reporting](https://github.com/sercanatalik/cereyan/security/advisories/new)
for this repository. Include what you found, how to reproduce it, and what an attacker could
do with it. You can expect an acknowledgement within a week.

## Scope worth knowing before you report

Cereyan is local-first by design, and some of what might look like a vulnerability is a
documented choice:

- The server binds `127.0.0.1:4200` by default and has no authentication unless an API token
  is set. Exposing it on a public interface without a token is a deployment decision, not a
  defect — see [Secure the server](https://sercanatalik.github.io/cereyan/guides/secure-the-server/).
- Flows and tasks are arbitrary Python that you supply, executed in engine child processes.
  Anyone who can register a flow can run code as the serving user, by design.
- The store is a local SQLite file whose permissions are the operating system's to enforce.
  Variables marked secret are encrypted at rest; nothing else is.

Reports about the token check, the Unix socket permissions, the secret encryption, the
custom-route surface, or the MCP tool surface are in scope and welcome.
