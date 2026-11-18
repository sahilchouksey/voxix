# voxix service packaging

Templates and guidance for running the voxix transcription service as an
unattended, no-display daemon. SDK plan §12 (M5 — GUI + packaging), §15 G8/G22
(process policy), §15 G2/G3 (discovery document).

Contents:

- `systemd/voxix-serve.service` — systemd **user** unit (Linux)
- `launchd/dev.xix3r.voxix-serve.plist` — launchd **user agent** (macOS)

Both ship as templates with documented placeholders (`__VOXIX_BIN__`,
`__LOG_DIR__`). They contain no fabricated paths for your machine: substitute
the real absolute path of the installed `voxix` binary before installing.

## What the daemon is (and is not)

- Headless: no window, tray, overlay, mic, hotkey, paste, or accessibility
  initialisation. It binds `127.0.0.1:47832` by default (`--bind IP:PORT` to
  change; `--data-dir PATH` for an isolated instance).
- First start prints the service token once (stderr when interactive, or
  `--token-stdout always`). The daemon stores only the verifier hash and
  publishes `discovery.json` (0600) with `port`, `pid`, `protocol_version`,
  and `token_fingerprint`. The plaintext never touches disk.
- Lost-token recovery is **not** automatic. If the token is lost, delete
  `state/service-auth.json` and restart the daemon to mint a fresh one
  (existing clients must be re-paired), or use the desktop app's Service tab,
  which can rotate the token while the daemon is running and the current
  token is held in memory.
- One process, one listener, one inference lease. A second `serve` against
  the same data directory exits with code 73 (lock/discovery contention).

## Linux — systemd user service

1. `cp systemd/voxix-serve.service ~/.config/systemd/user/`
   (after replacing `__VOXIX_BIN__`)
2. `systemctl --user daemon-reload && systemctl --user enable --now voxix-serve.service`
3. `systemctl --user status voxix-serve.service` → `active (running)`
   (`Type=notify`: systemd reports ready only after the daemon's `READY=1`)
4. Verify without credentials: `curl -fsS http://127.0.0.1:47832/alive`
   → `{"alive":true}`
5. Inspect discovery: `cat "$XDG_DATA_HOME/dev.xix3r.voxix/state/discovery.json"`
   (or `~/.local/share/dev.xix3r.voxix/state/discovery.json`)
6. Logs: `journalctl --user -u voxix-serve.service`

## macOS — launchd user agent

1. `cp launchd/dev.xix3r.voxix-serve.plist ~/Library/LaunchAgents/`
   (after replacing `__VOXIX_BIN__` and `__LOG_DIR__`)
2. `launchctl load -w ~/Library/LaunchAgents/dev.xix3r.voxix-serve.plist`
3. `launchctl list | grep voxix` — expect a pid and exit status 0;
   `curl -fsS http://127.0.0.1:47832/alive` → `{"alive":true}`

`ProcessType=Interactive` keeps the agent out of background/eco scheduling
classes (G8). Do not change it to `Background`.

## Reboot / no-display acceptance (G8)

On both platforms the unit is expected to pass **without any display or user
session UI**: at boot (systemd default target / launchd load) the daemon
starts, `/alive` answers, and authenticated routes answer 401 without a
bearer. CI/packaging should assert exactly that on a headless runner; the
process policy self-check (`voxix serve --check-process-policy`) also runs at
daemon startup and refuses a background-throttled QoS class.

## Exposing the service beyond loopback (TLS/VPN)

Default bind is loopback and bearer auth is not private on a plain LAN. If
you need remote access:

- **Preferred:** keep `127.0.0.1:47832` and front it with a TLS-terminating
  reverse proxy on the same host (Caddy, nginx, or a Tailscale/WireGuard
  tunnel). Terminate TLS, proxy to loopback, and require the bearer token at
  the proxy or let the daemon verify it — never send the bearer over an
  unencrypted link on an untrusted network.
- **Alternative:** bind `0.0.0.0:47832` only on a trusted VPN interface, not
  on a shared LAN, and still expect clients to present the bearer.
- WS `?ticket=` one-use tickets exist for runtimes that cannot set headers;
  they expire after 60 s and are single-use. Long-lived tokens belong in
  headers.
- Never publish the daemon port through a router/NAT port-forward without
  TLS in front of it.

## Updating the binary

Stop the daemon (`systemctl --user stop voxix-serve.service` /
`launchctl unload …`), replace the binary atomically (write to a temp file,
`rename` over the old path), start it again. `restart_required` in
`/v1/status` signals an updater-driven rotation.

## Signed macOS releases

Firewall acceptance of **signed** release artifacts on a clean Mac remains a
manual gate for the release pipeline (it needs the release artifact and a
clean machine); it is tracked as a deferred acceptance item in the M5
ledger, not as something this template can assert.
