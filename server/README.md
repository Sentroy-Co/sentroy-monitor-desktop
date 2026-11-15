# Self-host the Monitor server

One container plus MongoDB. You don't need a Sentroy account; you get your own users, workspaces,
single sign-on and screenshot storage.

## Start

```bash
mkdir sentroy-monitor && cd sentroy-monitor
curl -fsSLO https://raw.githubusercontent.com/Sentroy-Co/sentroy-monitor-desktop/main/server/docker-compose.yml
cat > .env <<EOF
MONITOR_PUBLIC_URL=https://monitor.example.com
MONITOR_ENCRYPTION_KEY=$(openssl rand -base64 32)
EOF
docker compose up -d
```

Put a TLS reverse proxy (Caddy, Traefik, nginx) in front of `127.0.0.1:3014`, then open
`https://monitor.example.com/setup` and create the first administrator. Setup closes for good once
an administrator exists. To keep strangers out of setup, set `MONITOR_SETUP_TOKEN` to a long random
value before the first start, and remove it afterwards.

In the desktop app, enter your server URL on the sign-in screen.

## Administration

Open it from the user menu → **Administration**.

| Page | What you do there |
|---|---|
| **Users** | Invite people (7-day links), disable accounts, revoke sessions and devices, create password-reset links |
| **Workspaces** | Create workspaces and set owner / admin / member roles. Members only see their own data. |
| **Sign-in** | OpenID Connect: Google Workspace, Microsoft Entra ID, Keycloak, Authentik, Okta… |
| **Storage** | Where screenshots go: Sentroy Storage (one click), any S3-compatible bucket, Firebase Storage, or the local volume |

Screenshots are off until a workspace turns them on, so you can skip storage at first.

## Environment

| Variable | | Purpose |
|---|---|---|
| `MONITOR_PUBLIC_URL` | required | Public origin, used for invite links, the SSO callback and desktop sign-in |
| `MONITOR_ENCRYPTION_KEY` | required | Encrypts secrets saved in the admin UI (AES-256-GCM). Losing it means re-entering them. |
| `MONITOR_SETUP_TOKEN` | optional | Locks `/setup` until the first administrator exists |
| `MONITOR_VERSION` | optional | Image tag, default `latest` |
| `MONITOR_STORAGE` + provider variables | optional | Storage from the environment instead of the admin UI |
| `MONITOR_OIDC_ISSUER`, `_CLIENT_ID`, `_CLIENT_SECRET` | optional | SSO from the environment instead of the admin UI |

## Updating

```bash
docker compose pull && docker compose up -d
```

The image is [`ghcr.io/sentroy-co/sentroy-oss-monitor`](https://github.com/orgs/Sentroy-Co/packages/container/package/sentroy-oss-monitor).
It is the same build that runs monitor.sentroy.com.
