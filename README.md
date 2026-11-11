# hue-mcp

Serveur [MCP](https://modelcontextprotocol.io) en Rust pour piloter un pont Philips Hue.

## État

Squelette du projet : le serveur expose un unique outil `ping` en transport
Streamable HTTP. Le contrôle du pont Hue, le TLS et l'authentification restent à faire.

## Lancer

```bash
cargo run
```

Le serveur écoute sur `http://127.0.0.1:8080/mcp`.

| Variable | Défaut | Rôle |
| --- | --- | --- |
| `HUE_MCP_BIND` | `127.0.0.1:8080` | Adresse d'écoute (`ip:port`) |
| `RUST_LOG` | `info` | Niveau de log |

Tant qu'il n'y a ni TLS ni authentification, ne pas exposer le serveur
au-delà de la boucle locale.

## Développement

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Licence

Distribué sous licence [GNU GPL v3](LICENSE) ou ultérieure.
