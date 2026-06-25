# hermes-dms

A compositor-native [Hermes](https://github.com/NousResearch/hermes) bridge for
**DankMaterialShell** on **Niri** (rh-anine). It makes the physical desktop a
**first-class Hermes gateway platform** alongside Telegram, email, and the
browser — sharing one agent identity, memory, and skill set.

There are two integration paths into Hermes:

1. **Desktop platform plugin** — a small Python adapter (`hermes-plugin/desktop/`)
   that runs *inside* the Hermes pod and dials out to the daemon's `/gateway`
   WebSocket. Panel chats flow through the **full Hermes gateway pipeline**, so
   the desktop gets slash commands (`/model`), per-session model switching, and
   shared memory — exactly like Telegram.
2. **MCP server** (`:9721/mcp`) — Hermes calls desktop **tools**
   (`desktop_notify`, `desktop_launch_app`, `desktop_screenshot`) from any
   surface.

The daemon also exposes a **Unix-socket IPC server**
(`$XDG_RUNTIME_DIR/hermes-dms.sock`) so the local QML plugins and
`hermes-dms-ctl` can talk to it via newline-delimited JSON. (A direct Hermes
REST client over `…/direct` remains as a fallback for when no platform adapter
is connected.)

```
┌──────────────── hr-main: Hermes pod (k8s) ────────────────┐
│  Hermes gateway ── loads hermes-plugin/desktop (adapter) ──│
│         ▲  │                                               │
└─────────┼──┼───────────────────────────────────────────────┘
  /gateway │  │ /mcp                 (WS + HTTP on :9721, Bearer-authed)
     WS    │  ▼
┌──────────────── rh-anine: hermes-dms daemon ──────────────┐
│  :9721  /gateway (WS) + /mcp     IPC: hermes-dms.sock      │
│     │                               ▲                      │
│     └── D-Bus / Niri (notify, launch_app, screenshot)      │
└─────────────────────────────────────┼──────────────────────┘
   hermesPanel / hermesLauncher (QML) ─┘ (via hermes-dms-ctl)
```

**Three installable pieces, in three places:**

| Piece | Source | Installs to |
|-------|--------|-------------|
| Daemon (`hermes-dms`, `hermes-dms-ctl`) | `src/` (Rust) | `~/.local/bin` + a systemd user service on rh-anine |
| DankMaterialShell plugins | `dms-plugins/` (QML) | `~/.config/DankMaterialShell/plugins/` |
| Hermes desktop platform plugin | `hermes-plugin/desktop/` (Python) | the Hermes pod's `/opt/data/plugins/` |

The Hermes-side network wiring (a Traefik route exposing the Hermes API to the
workstation, plus its NetworkPolicy) lives in the hr-fleet repo
(`fleet/ai/hermes-api-ingress.yaml`).

## Installation

### 1. Daemon (on rh-anine)

```bash
cargo install --path . --root ~/.local      # installs hermes-dms + hermes-dms-ctl

mkdir -p ~/.config/hermes-dms
$EDITOR ~/.config/hermes-dms/config.toml     # see "Configuration" below

cp contrib/hermes-dms.service ~/.config/systemd/user/
systemctl --user enable --now hermes-dms.service
systemctl --user status hermes-dms.service
```

Verify the daemon is up and the bridge route is reachable (a `401` means the
route exists and is Bearer-gated — exactly right):

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://10.20.0.3:9721/gateway   # → 401
hermes-dms-ctl status
```

### 2. DankMaterialShell plugins (on rh-anine)

Two QML plugins live in `dms-plugins/`:

- **hermesPanel** (`type: widget`) — a floating chat `PanelWindow` with a dankbar
  pill, a session switcher, a model picker, and a keyboard-toggle `IpcHandler`.
  It runs `hermes-dms-ctl stream` as a persistent relay (streaming replies, tool
  progress, and connection status).
- **hermesLauncher** (`type: launcher`, trigger `@`) — fire-and-forget one-shot
  commands; the reply arrives as a desktop notification from the daemon.

Symlink them into the DMS plugins directory, then enable them in
DankMaterialShell's plugin settings:

```bash
ln -sfn "$PWD/dms-plugins/hermesPanel"    ~/.config/DankMaterialShell/plugins/hermesPanel
ln -sfn "$PWD/dms-plugins/hermesLauncher" ~/.config/DankMaterialShell/plugins/hermesLauncher
```

Bind a key to toggle the panel (Niri `binds.kdl`):

```kdl
Alt+F10 hotkey-overlay-title="Roci" { spawn "dms" "ipc" "call" "hermesPanel" "toggle"; }
```

Both require `hermes-dms-ctl` on `PATH` and a running daemon.

### 3. Hermes desktop platform plugin (in the Hermes pod)

The adapter runs **inside the Hermes pod** and dials *out* to the daemon's
`/gateway` WebSocket (pod → rh-anine over VLAN20). Hermes loads file-based
platform plugins from `<HERMES_HOME>/plugins/` — here `/opt/data/plugins/` —
gated by a `plugins.enabled` allow-list.

```bash
POD=$(kubectl get pod -n ai -l app=hermes -o name | head -1); POD=${POD#pod/}

# Copy the plugin in and fix ownership (Hermes runs as uid/gid 10000).
kubectl cp hermes-plugin/desktop "ai/$POD:/opt/data/plugins/desktop" -c hermes
kubectl exec -n ai "$POD" -c hermes -- chown -R 10000:10000 /opt/data/plugins/desktop
```

Add the platform to the pod's `/opt/data/config.yaml` (the live config is the
source of truth; the Fleet ConfigMap is only an existence-gated first-boot seed,
so this edit persists across pod restarts):

```yaml
plugins:
  enabled:
    - desktop-platform              # the plugin's manifest name (plugin.yaml)
platforms:
  desktop:
    enabled: true
    extra:
      url: "ws://10.20.0.3:9721/gateway"   # the daemon's bridge
      token: "<same value as the daemon's mcp_auth_token>"
```

Reload the gateway process (**not** a pod restart — that triggers a slow
agent-tree chown):

```bash
kubectl exec -n ai "$POD" -c hermes -- /command/s6-svc -r /run/service/gateway-default
```

Verify: the daemon logs `desktop platform adapter connected`, and
`hermes gateway status` lists `desktop` among the connected platforms.

Notes:
- The adapter **trusts every sender** on this platform (no per-DM pairing) and
  seeds a home channel — it's a single-user, network-isolated, Bearer-authed
  bridge. It does this by seeding `DESKTOP_ALLOW_ALL_USERS` / `DESKTOP_HOME_CHANNEL`
  in-process; set `extra.allow_all: false` to opt out.
- The `token` must equal the daemon's `mcp_auth_token` — `/gateway` reuses the
  same Bearer layer as `/mcp`.
- A fresh-cluster rebuild (wiped `/opt/data`) re-runs steps 3 above; the plugin
  source is version-controlled here, so it's reproducible.

## Configuration

Daemon config lives at `~/.config/hermes-dms/config.toml` (mode `0600`). Secrets
can instead be supplied via `~/.config/hermes-dms/env` (the systemd unit reads it
as an `EnvironmentFile`), which keeps them out of the NFS-mounted `~/.config`.

```toml
# Hermes platform API server, via a Traefik route that stripPrefixes
# /direct -> hermes:8642. Used by the REST fallback path.
hermes_api_url  = "https://hermes.hr-home.xyz/direct"

# Same value as platforms.api_server.key in the Hermes pod's config.yaml.
# Prefer the HERMES_API_KEY env var (see env file) to keep it off NFS.
hermes_api_key  = "…"

# Bearer token guarding BOTH /mcp and /gateway. Hermes presents it from
# mcp_servers.desktop.headers and platforms.desktop.extra.token. Prefer the
# MCP_AUTH_TOKEN env var.
mcp_auth_token  = "…"

# Bind to the VLAN20 IP (not 0.0.0.0) — network isolation is the primary
# defense for the unauthenticated desktop tools.
mcp_listen_addr = "10.20.0.3:9721"

# Local tmpfs; never NFS.
socket_path     = "/run/user/1000/hermes-dms.sock"
```

`HERMES_API_KEY` / `MCP_AUTH_TOKEN` in the environment override the file values.

## CLI

```bash
hermes-dms-ctl status                 # daemon + Hermes connection status
hermes-dms-ctl chat "open firefox"    # one-shot; prints the streamed reply
hermes-dms-ctl stream                 # full-duplex JSON-lines bridge (used by the panel)
```

## Security model

- The daemon has **no k8s presence**. Both directions are Bearer-authed on
  `:9721`: Hermes dials the workstation directly for `/mcp` (tools) and
  `/gateway` (the platform bridge), using `mcp_auth_token`. The MCP server's
  `allowed_hosts` is derived from the bind address (the rmcp default is
  localhost-only and would reject Hermes on the VLAN20 IP).
- The desktop platform **trusts all senders** (allow-all) — appropriate for a
  single-user, network-isolated, Bearer-authed bridge, not for an
  internet-exposed surface.
- `desktop_launch_app` runs arbitrary commands (an accepted risk, gated by
  Hermes's single-user restriction) and **never goes through a shell**
  (`Command::new(cmd).args(...)`, no `sh -c`, no expansion).

## Notes

- **Screenshots use niri's built-in IPC** (no `grim` dependency); single-window
  shots are pixel-accurate.
- **`config.toml` on NFS:** `~/.config` is NFS-mounted from hr-main on rh-anine.
  Only rh-anine runs the daemon, so there's no collision, but prefer the env file
  for secrets.
- The desktop platform and its config persist on the pod's `/opt/data` volume,
  the same way every other Hermes platform here does.

## Testing

```bash
cargo test                 # unit + wiremock-backed integration tests
cargo clippy --all-targets # no-prod-unwrap lint is enforced (deny outside tests)
```

The daemon and `hermes-dms-ctl` run on bare metal, not in a container, so plain
`cargo test` on the host is correct.
