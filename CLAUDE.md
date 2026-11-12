# CLAUDE.md

Guidance for Claude Code (and any contributor) working in this repository.

## Language

**Everything in this repository is written in English**: code, identifiers, comments,
doc comments, log and error messages, tool descriptions, tests, README and other docs,
commit messages, branch names, pull request titles and descriptions, and review replies.
This holds even when the conversation with the maintainer happens in another language.

## Project

`hue-mcp` is an [MCP](https://modelcontextprotocol.io) server in Rust (edition 2024)
that controls a Philips Hue bridge. It serves MCP over the Streamable HTTP transport
(`rmcp` + `axum`) at `http://127.0.0.1:8080/mcp` by default. See [README.md](README.md)
for the user-facing configuration.

Current tools: `ping` and `bridge_status`. Light control, TLS and authentication of the
MCP server itself are not implemented yet.

## Layout

- `src/main.rs`: entry point. Loads `.env`, sets up logging, reads `HUE_MCP_BIND`,
  builds the `HueServer` and serves it with axum.
- `src/config.rs`: `Config` read from environment variables (`HUE_BRIDGE_HOST`,
  `HUE_APP_KEY`, `HUE_BRIDGE_ID`). `Config::from_lookup` takes a lookup closure so tests
  never touch the real environment. `AppKey` wraps the secret and hides it from `Debug`.
- `src/server.rs`: the MCP server (`HueServer`) and its tools, declared with the `rmcp`
  `#[tool_router]` / `#[tool]` macros. Handles a configured bridge address or one found
  through mDNS (cached, shared by all sessions, searched again when the bridge stops
  answering). Discovery and client creation are injected (`FindFn`, `ConnectFn`) so they
  can be replaced in tests.
- `src/hue/client.rs`: `HueClient`, the HTTPS client for the bridge, and `HueError`.
- `src/hue/tls.rs`: rustls certificate verifier trusting only the Hue root CAs embedded
  from `certs/`, optionally requiring the certificate to match a given bridge id.
- `src/hue/discovery.rs`: mDNS discovery of the `_hue._tcp.local.` service (`mdns-sd`).
- `src/hue/testing.rs` (test only): a fake bridge built on `wiremock`, plus helpers.

## Commands

```bash
cargo build
cargo run
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI (`.github/workflows/ci.yml`, `ubuntu-latest` only) runs `fmt --check`, `clippy` with
warnings denied and `test` on every pull request and push to `main`. Run all three
before committing. Keep CI on Ubuntu runners only: do not add Windows or macOS jobs.

## Conventions

- **Secrets**: the application key must never appear in logs, error messages, `Debug`
  output or tool results. Keep it in `AppKey` and only call `expose()` where the key is
  actually sent to the bridge. Never commit `.env`. Tests check this
  (`debug_output_never_contains_the_key`, `errors_never_contain_the_key`,
  `status_never_contains_the_key`); add similar
  tests when you add code paths that handle the key.
- **Bridge trust**: the HTTP client only trusts the embedded Hue root CAs and does not
  follow redirects. Do not loosen certificate checks; a discovered bridge must present a
  certificate matching its announced id.
- **Errors**: library-style modules use `thiserror` enums (`HueError`, `DiscoveryError`);
  `main` uses `anyhow` with `.context(...)`. Messages returned to MCP clients should say
  what is wrong and what to do about it (which variable to set, what to check).
- **Tools**: tool results are structured (`Json<...>` with `JsonSchema` types). Document
  fields with doc comments, since they end up in the schema seen by clients.
- **Tests**: unit tests live next to the code in `#[cfg(test)] mod tests`. They must not
  need real hardware or the network: use `hue::testing::fake_bridge()` and inject fakes
  instead of calling mDNS. Async tests use `#[tokio::test]`.
- **Comments**: explain why, not what. Keep them short.
- **Security default**: the server binds to loopback by default because there is no TLS
  or authentication yet. Keep that default until both exist.

## Git and pull requests

- Never commit directly to `main`: work on a branch (`feat/...`, `fix/...`, `docs/...`,
  `chore/...`) and open a pull request.
- Commit messages follow Conventional Commits with a scope, e.g.
  `feat(bridge): discover the bridge via mDNS`, `chore(ci): ...`, `docs: ...`.
- Pull requests are squash-merged. Their description has a **Rationale**, a **Summary**
  of the changes and a **Tests** checklist (automated checks and, when relevant, manual
  checks against a real bridge).
- Update `README.md` in the same pull request when behaviour or configuration changes.
