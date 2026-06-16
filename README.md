# hermes-dms

A compositor-native [Hermes](https://github.com/NousResearch/hermes) bridge for
**DankMaterialShell** on **Niri** (rh-anine). It makes the physical desktop a
first-class Hermes surface alongside Telegram, email, and the browser — sharing
one agent identity, memory, and skill set.

`hermes-dms` is a user-level daemon with three interfaces:

1. **MCP server** (StreamableHTTP, port 9721) — Hermes calls desktop tools
   (`desktop_notify`, `desktop_launch_app`, `desktop_screenshot`) from any
   surface.
2. **Hermes REST client** — the desktop sends chat messages to the Hermes API
   server (port 8642) and streams replies.
3. **Unix-socket IPC server** (`$XDG_RUNTIME_DIR/hermes-dms.sock`) — local QML
   plugins and `hermes-dms-ctl` talk to the daemon via newline-delimited JSON.

```
┌──────────────────────────────────────────────────────────┐
│ rh-anine (bare metal, user session)                        │
│                                                            │
│  QML plugins ──unix socket──► hermes-dms ──REST──► Hermes  │
│  hermes-dms-ctl                  │  ▲   (10.20.0.3)  :8642  │
│                                  │  └── MCP :9721 ◄── Hermes│
│                                  └── D-Bus / Niri / grim    │
└──────────────────────────────────────────────────────────┘
```

This repo contains the **daemon** (Rust, `src/`) and the two **DankMaterialShell
QML plugins** (`dms-plugins/`). The Hermes-side wiring (an IngressRoute exposing
the Hermes API to the workstation, plus its NetworkPolicy) lives in the hr-fleet
repo (`fleet/ai/hermes-api-ingress.yaml`).

## Build & install

```bash
cargo build --release
cargo install --path . --root ~/.local   # installs hermes-dms + hermes-dms-ctl
```

Run as a systemd user service:

```bash
cp contrib/hermes-dms.service ~/.config/systemd/user/
systemctl --user enable --now hermes-dms.service
```

## DankMaterialShell plugins

Two QML plugins live in `dms-plugins/`:

- **hermesPanel** (`type: widget`) — a floating chat `PanelWindow` with a
  dankbar pill and a keyboard-toggle `IpcHandler` (`dms ipc call hermesPanel
  toggle`). It runs `hermes-dms-ctl stream` as a persistent relay, streaming
  deltas/tool-progress and showing connection status.
- **hermesLauncher** (`type: launcher`, trigger `@`) — fire-and-forget one-shot
  commands; the reply arrives as a desktop notification delivered by the daemon.

Install by symlinking them into the DMS plugins directory, then enable them in
DankMaterialShell's plugin settings:

```bash
ln -sfn "$PWD/dms-plugins/hermesPanel"    ~/.config/DankMaterialShell/plugins/hermesPanel
ln -sfn "$PWD/dms-plugins/hermesLauncher" ~/.config/DankMaterialShell/plugins/hermesLauncher
```

Both require `hermes-dms-ctl` on `PATH` and a running daemon.

## Configuration

Config lives at `~/.config/hermes-dms/config.toml` (mode `0600`). All fields are
optional except the API key.

```toml
# Hermes platform API server. The address reachable from bare-metal rh-anine
# must be confirmed at deploy time (see "Deployment notes").
hermes_api_url  = "https://hermes.hr-home.xyz/direct"

# Same value as platforms.api_server.key in the Hermes pod's /opt/data/config.yaml.
# Prefer the HERMES_API_KEY env var (systemd EnvironmentFile) to keep it off NFS.
hermes_api_key  = "…"

# Bind the MCP server to the VLAN20 IP (not 0.0.0.0) — network isolation is the
# primary defense for the unauthenticated desktop tools.
mcp_listen_addr = "10.20.0.3:9721"

# Local tmpfs; never NFS.
socket_path     = "/run/user/1000/hermes-dms.sock"
```

`HERMES_API_KEY` in the environment overrides `hermes_api_key` from the file.

## CLI

```bash
hermes-dms-ctl status                 # daemon + Hermes connection status
hermes-dms-ctl chat "open firefox"    # one-shot; prints the streamed reply
hermes-dms-ctl stream                 # full-duplex JSON-lines bridge (panel)
```

## Security model

- The daemon has **no k8s presence**. Hermes reaches the MCP server by dialing
  the workstation directly at `http://10.20.0.3:9721/mcp` — the host:port set in
  Hermes's `mcp_servers.desktop` config (config-based registration). The endpoint
  is protected by a **Bearer token** (`mcp_auth_token`); Hermes sends it via
  `mcp_servers.desktop.headers.Authorization`. The daemon reaches the Hermes API
  in the other direction via `https://hermes.hr-home.xyz/direct` (a Traefik
  route that stripPrefixes `/direct` → `hermes:8642`). `desktop_launch_app` runs arbitrary commands —
  an accepted risk gated by Hermes's single-user Telegram restriction.
- `desktop_launch_app` never goes through a shell (`Command::new(cmd).args(...)`,
  no `sh -c`, no expansion).
- The MCP server's `allowed_hosts` is derived from the bind address (the rmcp
  default is localhost-only and would reject Hermes on the VLAN20 IP).

## Deployment notes / deferred items

- **Hermes API URL from bare metal:** rh-anine is a k3s node, so it has flannel
  routes to pod CIDRs and kube-proxy ClusterIP DNAT. Confirm which address is
  reachable (ClusterIP DNS, pod IP, or a LAN IP) before relying on the default.
- **`grim` is required** for `desktop_screenshot`: `emerge gui-apps/grim`.
- **Single-window screenshots** capture the focused *output* (monitor). Pixel
  accurate per-window cropping is deferred (P1) — niri's per-window geometry is
  workspace-view-relative, not the global coordinates `grim -g` expects.
- **Live DMS validation pending:** the QML plugins are written against the
  proven dms-agent/commandRunner patterns but have not yet been loaded in a
  running DankMaterialShell — enable them and verify the panel toggle, streaming,
  and launcher notification end-to-end.
- **`config.toml` on NFS:** `~/.config` is NFS-mounted from hr-main on rh-anine.
  Only rh-anine runs the daemon, so there's no collision, but prefer the
  `HERMES_API_KEY` env var to keep the secret off NFS.

## Testing

```bash
cargo test          # unit + wiremock-backed integration tests
cargo clippy --all-targets
```

The daemon and `hermes-dms-ctl` run on bare metal, not in a container, so plain
`cargo test` on the host is correct (unlike the hr-fleet Rust *containers*).
