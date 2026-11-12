# hue-mcp

Serveur [MCP](https://modelcontextprotocol.io) en Rust pour piloter un pont Philips Hue.

## État

Le serveur sait joindre le pont Hue et s'y authentifier. Il expose deux outils en
transport Streamable HTTP :

| Outil | Rôle |
| --- | --- |
| `ping` | Vérifie que le serveur MCP répond. |
| `bridge_status` | Diagnostic de la connexion au pont : joignable, clé acceptée, nom, modèle, identifiant et version. |

Le pilotage des lumières, la découverte mDNS du pont, le TLS et l'authentification
du serveur MCP restent à faire.

## Configuration

Les réglages se font par variables d'environnement. Un fichier `.env` à la racine
est chargé s'il existe ; les variables d'environnement réelles restent prioritaires.

| Variable | Défaut | Rôle |
| --- | --- | --- |
| `HUE_BRIDGE_HOST` | — | Adresse du pont (`ip` ou `ip:port`). |
| `HUE_APP_KEY` | — | Clé d'application Hue. Secrète : ne pas la committer. |
| `HUE_BRIDGE_ID` | — | Identifiant du pont (ex. `ecb5fafffe13af9c`). S'il est défini, le certificat du pont doit lui correspondre. |
| `HUE_MCP_BIND` | `127.0.0.1:8080` | Adresse d'écoute du serveur MCP (`ip:port`). |
| `RUST_LOG` | `info` | Niveau de log. |

Sans `HUE_APP_KEY`, le serveur démarre quand même ; `bridge_status` indique alors
que la clé manque.

Tant qu'il n'y a ni TLS ni authentification, ne pas exposer le serveur MCP
au-delà de la boucle locale.

### Créer une clé d'application

1. Appuyer sur le bouton rond du pont.
2. Dans les 30 secondes, envoyer la requête suivante (remplacer l'adresse du pont) :

```bash
curl -k -X POST https://192.168.1.7/api -d '{"devicetype":"hue-mcp#serveur","generateclientkey":true}'
```

3. La réponse contient la clé dans le champ `username` : la placer dans `HUE_APP_KEY`.

Si la réponse est `link button not pressed`, le délai est dépassé : recommencer.
L'option `-k` est nécessaire car `curl` ne connaît pas l'autorité de certification
privée des ponts Hue ; le serveur, lui, vérifie le certificat (voir ci-dessous).

### Sécurité de la liaison avec le pont

Le serveur parle au pont en HTTPS et n'accepte que les certificats émis par les
autorités racines Hue embarquées dans [certs/](certs). Définir `HUE_BRIDGE_ID`
restreint en plus la connexion à ce pont précis. La clé d'application n'est jamais
écrite dans les logs ni renvoyée par les outils.

## Lancer

```bash
cargo run
```

Le serveur écoute sur `http://127.0.0.1:8080/mcp`.

## Développement

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Ces trois commandes sont exécutées par la CI GitHub Actions (Ubuntu) sur chaque pull request et chaque push vers `main`.

Les tests utilisent un faux pont local et ne nécessitent aucun matériel.

## Licence

Distribué sous licence [GNU GPL v3](LICENSE) ou ultérieure.
