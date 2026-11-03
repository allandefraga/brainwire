# Security

Brainwire is an experimental beta. Use the newest published beta when evaluating
it, and read its [release notes](https://github.com/allandefraga/brainwire/releases).
Older prereleases do not have a separate security maintenance commitment.

## Report a suspected vulnerability

Do not post exploit details, credentials or private data in a public issue.
Use the private [GitHub vulnerability reporting channel](https://github.com/allandefraga/brainwire/security/advisories/new)
to contact the maintainers. If it is unavailable, open an issue containing only
a request for a private security contact, without the vulnerability details.
Do not send a real database volume or live tokens.

A useful private report includes the affected version, deployment mode,
preconditions, expected impact and a minimal synthetic reproduction. There is
no guaranteed response or patch timeline during the beta.

## Deploy with an explicit trust boundary

- The [local demo](README.md#quickstart) deliberately disables authentication and
  publishes its port on host loopback. It is intended for local experiments.
- For shared access, configure [authentication and grants](docs/bolt-auth.md),
  use [protected containers](docs/protected-containers.md) and follow the
  [encrypted transport guide](docs/protected-transport.md). Native Bolt TLS is
  not implemented.
- Give applications only their required graph and operation permissions. Keep
  administrator credentials separate from application and health credentials.
- Keep secret files private, redact logs before sharing and use synthetic data
  in issue reproductions. Never commit credentials or production datasets.
- Verify [backup and restoration](docs/checkpoints.md), resource limits and the
  [production readiness gates](docs/production-readiness-roadmap.md) for your
  workload. Experimental quorum does not establish production multi-host HA.

The project does not claim a completed independent security audit. The
[current limits](README.md#current-limits) are part of the beta contract.
