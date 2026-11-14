# Security Policy

## Reporting a vulnerability

Please do not open a public issue for a security problem. Report it privately through
GitHub's "Report a vulnerability" button on the repository's Security tab.

Include the affected version or commit, the configuration involved (for example whether the
server runs behind a reverse proxy), and steps to reproduce.

## Scope

Campfire is a single-node, in-memory runtime. Deployment notes that affect security are in
[docs/configuration.md](docs/configuration.md) and [docs/dashboard.md](docs/dashboard.md):

- TLS is terminated by a reverse proxy; the server itself serves plain HTTP.
- Register reverse proxies with `--trusted-proxy`, and only proxies that append or overwrite
  `X-Forwarded-For`. Otherwise per-source limits are shared or can be spoofed.
- The admin dashboard shares the game listener. Keep the bind address on loopback or a
  private network unless the proxy restricts `/admin`.
- Game modules are trusted, synchronous Rust code and are not sandboxed.
