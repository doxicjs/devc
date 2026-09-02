# Migration Guide

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
