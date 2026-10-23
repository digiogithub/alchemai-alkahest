# Running as a service

`alkahest serve` can run in the foreground, but a server you rely on should start at boot and
restart after a failure. `alkahest service` does that.

```sh
sudo alkahest service install      # system service, runs as the `alkahest` user
alkahest service install --user    # per-user service, no root needed
alkahest service status
alkahest service stop
alkahest service start
alkahest service uninstall
```

Add `--dry-run` to any install or uninstall to print the files it would write and the commands
it would run without changing anything.

## Linux (systemd)

**System service** (`sudo alkahest service install`):

- creates the system user `alkahest` (or reuses it) and the state directory `/var/lib/alkahest`,
  which becomes `ALKAHEST_HOME`;
- writes `/etc/systemd/system/alkahest.service` (`Restart=always`, `ExecStart=<binary> serve`,
  a stop timeout above the 30 s request drain) and runs `systemctl daemon-reload` and
  `enable --now`;
- adds the `render` and `video` groups (GPU access) and `ollama` (read access to a system Ollama
  store) when those groups exist;
- logs go to the journal: `journalctl -u alkahest -f`;
- the system configuration file `/etc/alkahest/config.toml` is read as usual.

**User service** (`--user`): a unit in `~/.config/systemd/user/` running as you, with your own
data directory. Run `loginctl enable-linger $USER` to keep it running when you are logged out.

`service uninstall` stops and removes the unit; the service user and its data stay, so
reinstalling does not lose models. `service status` prints whether it runs and the URL it
listens on, with exit code 3 when it is not active (like `systemctl is-active`); `--json` is
supported.

Options: `--user`, `--dry-run`, `--root <dir>` (write below a directory, for packaging and
tests) and `--bin <path>` (the binary the unit starts).

Packages (`.deb`, tar.gz) never install a service on their own; this command is the explicit
step.

### Models pulled before the system service

The system service has its own model store under `/var/lib/alkahest`. Models you pulled while
running `alkahest serve` or the desktop app as yourself stay in your store
(`~/.local/share/alkahest`), so once the service owns port 11435 they no longer show up in
`alkahest ls` or the desktop. Nothing is deleted. `service install` prints both things to check:

- **The port is in use.** A server you started yourself still holds 11435. Stop it, or the
  service keeps restarting without ever listening (`journalctl -u alkahest` shows
  `Address already in use`).
- **Your models and engines.** It prints the command that copies them into the service store:

```sh
sudo env ALKAHEST_HOME=/var/lib/alkahest alkahest import --store ~/.local/share/alkahest
```

`import --store` copies:

- every model, with the same digest, plus its per-model settings (`model-config/`);
- every engine installed in the source's `engines/` folder: downloaded builds and source builds,
  such as `prism-llama-cpp` for the Bonsai models.

It skips what the destination already has, and it leaves the source store unchanged. Run as root
into the service store, the new files go to the `alkahest` user. Two things are not copied:

- **Python venv engines (vLLM).** A venv only works at the path it was created at. Install them
  again with `sudo env ALKAHEST_HOME=/var/lib/alkahest alkahest engines install vllm`.
- **Engines, with `--no-engines`.** Pass it to copy only the models.

Once the copy is done, `alkahest rm` in your own store frees the space, but only while that store
is in use.

After upgrading the package, restart the service so it runs the new binary:
`sudo systemctl restart alkahest`. Packages never run maintainer scripts, so an upgrade does not
restart it for you.

## macOS and Windows

The same commands work on every platform; the manager is chosen for you (`--manager
systemd|launchd|windows` overrides it, and another platform's manager is only accepted together
with `--dry-run` or `--root`).

**macOS (launchd).** `sudo alkahest service install` creates the hidden `_alkahest` account and
a LaunchDaemon (`es.digio.alkahest`, `RunAtLoad`, `KeepAlive`) with its state in
`/Library/Application Support/Alkahest`. `alkahest service install --user` installs a
LaunchAgent in `~/Library/LaunchAgents` that runs as you and needs no root. Logs go to a file in
`~/Library/Logs/Alkahest` (user) or the service log directory.

**Windows.** Run `alkahest service install` from an elevated prompt: it registers the service
`alkahest` (delayed automatic start, runs as `LocalService`, restarts after failures) with its
state in `%ProgramData%\Alkahest`, and an Event Log source `Alkahest` for start, stop and failure
messages. Per-user services do not exist on Windows (`--user` is refused). Closing the service
(or killing it) ends its engine processes: they are tied to the server by a job object.

The setup scripts call `service install --user` when you pass `--service`.

## Changing the address or settings

Edit the configuration file (`sudo $EDITOR /etc/alkahest/config.toml` for the system service)
and restart: `sudo systemctl restart alkahest`. If you expose the service beyond loopback,
create an API key first ([Serving and security](serving.md)); the server refuses to start
otherwise.
