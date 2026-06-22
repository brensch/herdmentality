# Deploying Herdcore

`docker-compose.yml` (repo root) builds and runs the three services:

| service  | what it is                              | exposed?                |
|----------|-----------------------------------------|-------------------------|
| `server` | axum WebSocket game server (`/ws`)      | no — internal only      |
| `bot`    | bot service playing CPU seats           | no — internal only      |
| `web`    | Caddy serving the WASM frontend + `/ws` | yes — host port `8080`  |

Only `web` publishes a port. It serves the static Yew/WASM bundle (zstd/gzip
compressed, immutable caching on hashed assets) and reverse-proxies `/ws` to
`server` over the internal network, so the browser only ever talks to one origin.

## Build & run

```sh
docker compose up -d --build
```

The frontend connects to the game server **same-origin**: it opens a WebSocket to
`/ws` on whatever host served the page (`localhost:8080` in dev, the public domain
in prod), and Caddy proxies it to the server. So the same bundle works everywhere
— no per-domain rebuild. To force a specific server instead, build with
`--build-arg HERDCORE_WS_URL=wss://host/ws`.

The server's database is persisted in the `herdcore-data` volume.

Override the published port with `HERDCORE_WEB_PORT` (defaults to `8080`):

```sh
HERDCORE_WEB_PORT=9000 docker compose up -d
```

## External Caddy (lives in the `home` repo, not here)

TLS for `herdcore.snek2.ddns.net` is terminated by the existing Caddy on the home
box. Add this site block there, pointing at wherever this stack runs (use
`localhost:8080` if it's the same host):

```caddy
herdcore.snek2.ddns.net {
	reverse_proxy <stack-host>:8080
}
```

Caddy proxies WebSocket upgrades transparently, so `/ws` flows through without any
extra config. Nothing but the website is reachable from outside.
