# Migration Guide

## 0.3.0 → 0.3.1

Bug-fix release. No config changes.

### Fixes a 0.3.0 regression: devc outliving its terminal

**If you are on 0.3.0, upgrade.** Closing the terminal running devc left the process alive, spinning at ~95% CPU, with its services orphaned and its control socket stale.

Cause: 0.3.0 installed a `SIGHUP` handler that set a shutdown flag. But once the pty is gone, `crossterm::event::poll` spins on EOF reads and never returns, so the event loop never reached the check — the handler had replaced a guaranteed kernel termination with a flag nothing could read. Releases before 0.3.0 were unaffected because `SIGHUP` still had its default disposition (terminate immediately).

### Closing the terminal now stops your services

The fix is a shutdown watchdog on its own thread, so this is now *better* than pre-0.3.0 behavior rather than merely restored:

| | ≤ 0.2.0 | 0.3.0 | 0.3.1 |
| --- | --- | --- | --- |
| devc process | dies | **spins forever at ~95% CPU** | dies |
| services | orphaned | orphaned | **stopped** |
| control socket | left stale | left stale | **removed** |

If you relied on services surviving a closed terminal, they no longer do — detach with your terminal multiplexer instead, or leave devc running.

The watchdog only acts when the event loop is genuinely stuck: on a signal it waits briefly for the loop to *begin* shutting down, and stands down if it does, so an orderly shutdown still gets its full per-service grace period.

## 0.2.0 → 0.3.0

0.3.0 adds a control socket so scripts and agents can query and drive a running devc, and makes devc the single authority on whether a service is up. No config changes are required.

### A second `devc` now attaches instead of starting a second supervisor

Running `devc` in a project that already has one no longer gives you two independent devcs each spawning their own copy of every service. The second invocation attaches to the first as an extra view — like `tmux attach`.

Consequences:

- Selection and scroll are **shared**. Moving the cursor in the attached view moves it in the primary.
- `q` in an attached view detaches. It does not quit the primary or stop any services.
- If you relied on running two devcs against the same `devc.toml`, use separate config files (their sockets are keyed by the canonical config path).

### Services held by a foreign process are reported, not duplicated

If a service's `port` is already answering and devc has no process for it, the service now shows as `external` with the owning PID (`◆ Web:5173 [external pid 48213]`) instead of a bare `◆`.

- Starting it is a no-op that says who holds the port. It will not spawn a second copy.
- Stopping it is **refused** — devc didn't start it, so it won't kill it. Stop it yourself.

This only works for services with a `port` set. A service with no `port` has nothing observable to probe, so set `port` where you want this protection.

### `x` (stop-all) and toggling are unchanged

Existing keybindings behave the same. One internal change: the Services help line no longer advertises `a start all`, which stopped existing in 0.2.0.

### New: control subcommands

`status`, `start`, `stop`, `restart`, `run`, `logs`, and `ls` are now reserved as the first argument. If you have a config file literally named one of these, pass it with `--config`:

```bash
devc --config status    # instead of: devc status
```

See the README's "Driving devc from scripts and agents" section.

## 0.1.x → 0.2.0

0.2.0 tightens the config schema and the keybinding model. Most users won't touch their config — the changes show up as clearer error messages and a new `⚠ conflicts` badge when something's off.

### Schema: `service_type` removed

The `service_type` field is no longer accepted in `devc.toml` or `devc.local.toml`. It was never read — it had no effect on behavior — but `deny_unknown_fields` required it. Removing it cleans up the schema.

Delete every `service_type = "..."` line from your configs:

```bash
# macOS
sed -i '' '/^service_type/d' devc.toml
# Linux
sed -i '/^service_type/d' devc.toml
```

Repeat for `devc.local.toml` if present.

### Keybindings: `a` is no longer start-all

The global `a` shortcut is gone. `a` is now a free user binding on every tab. To start services:

- Press `Enter` on a selected service, or
- Bind each service to its own key and press that key.

Stop-all on `x` (Services tab only) is unchanged.

### Keybindings: stricter reserved-key detection

Binding any service, command, or tool to `q`, `j`, `k`, or `space` never actually worked — the event loop consumed those keys first. 0.2.0 now detects these in the config and surfaces them as a sticky `⚠ N conflicts` badge in the header, plus a detailed `warning: ...` line printed to stderr on exit.

If your badge lights up, rebind the flagged entry to any other key. No config changes are required if you weren't using these keys.

### UI: mouse wheel scrolls the log panel

In 0.1.x, mouse-wheel events arrived as synthesized arrow-key presses, which traversed the services/commands list. 0.2.0 captures mouse events explicitly and routes scroll to the log panel instead.

Side effect: native click-drag selection in the terminal is consumed by the TUI. Hold `Shift` (or `Option` in iTerm on macOS) for native selection.
