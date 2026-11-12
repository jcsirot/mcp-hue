# hue-mcp

An [MCP](https://modelcontextprotocol.io) server written in Rust to control a Philips Hue bridge.

## Status

The server can reach the Hue bridge and authenticate with it. It exposes two tools
over the Streamable HTTP transport:

| Tool | Purpose |
| --- | --- |
| `ping` | Checks that the MCP server is responding. |
| `bridge_status` | Diagnoses the connection to the bridge: reachable, where the address came from, key accepted, name, model, id and version. |

Light control, TLS and authentication of the MCP server itself are still to be done.

## Configuration

Settings are read from environment variables. A `.env` file at the repository root
is loaded if present; real environment variables take precedence.

| Variable | Default | Purpose |
| --- | --- | --- |
| `HUE_BRIDGE_HOST` | — | Bridge address (`ip` or `ip:port`). When unset, the bridge is looked up through mDNS. |
| `HUE_APP_KEY` | — | Hue application key. Secret: do not commit it. |
| `HUE_BRIDGE_ID` | — | Bridge id (e.g. `ecb5fafffe13af9c`). When set, the bridge certificate must match it, and mDNS discovery only keeps that bridge. |
| `HUE_MCP_BIND` | `127.0.0.1:8080` | Listen address of the MCP server (`ip:port`). |
| `RUST_LOG` | `info` | Log level. |

Without `HUE_APP_KEY`, the server still starts; `bridge_status` then reports that
the key is missing.

As long as there is neither TLS nor authentication, do not expose the MCP server
beyond the loopback interface.

### Bridge discovery

When `HUE_BRIDGE_HOST` is not set, the server looks for the `_hue._tcp` service
through mDNS on the first tool call, for at most 3 seconds. The address it finds is
remembered; if the bridge stops responding, a new search is started. If several
bridges answer, `bridge_status` lists them: set `HUE_BRIDGE_ID` to pick one.

The bridge certificate must match the announced id, so a device pretending to be a
bridge is rejected.

mDNS relies on multicast, which does not always go through a VPN or a virtual
machine's network. If discovery fails, set `HUE_BRIDGE_HOST` (a fixed address,
reserved in the router's DHCP settings, is then preferable).

### Creating an application key

1. Press the round button on the bridge.
2. Within 30 seconds, send the following request (replace the bridge address):

```bash
curl -k -X POST https://192.168.1.7/api -d '{"devicetype":"hue-mcp#server","generateclientkey":true}'
```

3. The response holds the key in the `username` field: put it in `HUE_APP_KEY`.

If the response is `link button not pressed`, the delay has expired: try again.
The `-k` option is needed because `curl` does not know the private certificate
authority of Hue bridges; the server, on the other hand, does verify the
certificate (see below).

### Security of the bridge connection

The server talks to the bridge over HTTPS and only accepts certificates issued by
the Hue root authorities embedded in [certs/](certs). Setting `HUE_BRIDGE_ID`
further restricts the connection to that specific bridge. The application key is
never written to the logs nor returned by the tools.

## Running

```bash
cargo run
```

The server listens on `http://127.0.0.1:8080/mcp`.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

These three commands are run by the GitHub Actions CI (Ubuntu) on every pull request
and every push to `main`.

The tests use a local fake bridge and do not need any hardware.

## License

Distributed under the [GNU GPL v3](LICENSE) or later.
