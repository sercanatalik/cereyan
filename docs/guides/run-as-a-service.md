# How to run the server as a service

Run `cereyan serve` under the operating system's service manager so schedules fire after a reboot and the server restarts if it dies. Engines survive a server restart: on start the supervisor adopts runs whose engine is still alive and marks the rest crashed and reruns them.

## What the service needs

- A working directory containing the flows (and `cereyan.toml`), passed to `cereyan serve`.
- `CEREYAN_HOME` set explicitly, so the home does not depend on which user's `~` is in effect.
- `CEREYAN_NO_BROWSER=1` or `--no-open`, since there is no display.
- The token in the environment if the API is protected.
- `Restart=always` or the equivalent; the server is safe to restart at any time.

## systemd (Linux)

`/etc/systemd/system/cereyan.service`:

```ini
[Unit]
Description=cereyan pipeline server
After=network.target

[Service]
User=pipelines
WorkingDirectory=/srv/pipelines
Environment=CEREYAN_HOME=/var/lib/cereyan
Environment=CEREYAN_NO_BROWSER=1
EnvironmentFile=-/etc/cereyan/env
ExecStart=/srv/pipelines/.venv/bin/cereyan serve /srv/pipelines --host 127.0.0.1 --port 4200
Restart=always
RestartSec=2
KillSignal=SIGTERM
TimeoutStopSec=60

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now cereyan
journalctl -u cereyan -f
```

Put `CEREYAN_TOKEN=...` in `/etc/cereyan/env` (mode 0600) rather than in the unit file. `TimeoutStopSec` should exceed `cancel_grace_secs` twice over, so a stop lets running work finish or be cancelled cleanly.

## launchd (macOS)

`~/Library/LaunchAgents/xyz.helixio.cereyan.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>xyz.helixio.cereyan</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/me/pipelines/.venv/bin/cereyan</string>
    <string>serve</string>
    <string>/Users/me/pipelines</string>
    <string>--no-open</string>
  </array>
  <key>WorkingDirectory</key><string>/Users/me/pipelines</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>CEREYAN_HOME</key><string>/Users/me/.cereyan</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/Users/me/Library/Logs/cereyan.log</string>
  <key>StandardErrorPath</key><string>/Users/me/Library/Logs/cereyan.log</string>
</dict>
</plist>
```

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/xyz.helixio.cereyan.plist
launchctl kickstart -k gui/$(id -u)/xyz.helixio.cereyan   # restart after editing flows' dependencies
tail -f ~/Library/Logs/cereyan.log
```

## Windows

Use Task Scheduler with a task that runs at logon or at startup, action `cereyan.exe serve C:\pipelines --no-open`, and "restart if the task fails". Set `CEREYAN_HOME` in the task's environment or system-wide.

## Logs

The server logs to standard error: one line per start with the address and auth state, warnings for unknown configuration keys, and errors from engines that fail to import. Run logs are in the database, not in the service log; read them in the UI, with `cereyan runs ls`, or through the API.

## Upgrading

Stop the service, install the new wheel into the same environment, start it. Migrations run on the first open; downgrades are noted in the [changelog](../changelog.md) when they need care.

## Picking up code changes

Engines are recycled when their module file changes, so editing a flow's module takes effect on the next run without a restart. Adding a new module, changing `cereyan.toml`, or changing a flow's schedule declaration needs a restart.

Related: [Engines and the home directory](../concepts/engines-and-home.md), [Secure the server](secure-the-server.md).
