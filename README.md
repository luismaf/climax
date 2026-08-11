# climax

> Claude Code quota guard.

`climax` watches your Claude Code rate-limit window through the official
JSON `statusLine` hook (no UI scraping) and wakes your agent(s) the moment
the window opens again. Optionally, right before your quota runs out, it
lets your main agent delegate its pending tasks to other dev agents, so
they keep working while you wait. Maximize your Claude usage.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/luismaf/climax/master/scripts/install.sh | bash
```

It detects your system (Ubuntu/Debian via apt, Arch via PKGBUILD, macOS or
other Linux as a binary in `~/.local/bin`, Windows via cargo) and never
touches your services or config. Pin a release with
`| bash -s -- -v 0.5.0`.

```bash
climax --install  # systemd user service, boot autorun (opt-in)
```

## Quick start

```bash
climax          # start the daemon (installs the service first if missing) and print status
climax -s       # read-only status: ON/OFF, service installed?, quota, hook, agents
climax -q       # stop the daemon (never uninstalls the service)
climax -d       # turn DELEGATION on
climax -n       # turn DELEGATION off
```

## DELEGATION

Off by default. Right before the window ends, `climax` asks the main agent
to hand its pending work over to the other herdr agents, so they keep
advancing while the main one is blocked. At the reset, the auto-resume
wakes everyone back up.

```bash
climax -d                  # on (default message)
climax -d 'delegate now'   # ... with a custom message (or trailing args)
climax -n                  # off
```

## Usage

```
MODES (no flags = start the daemon + status):
  (no flags) / -z, --start   start the daemon (installs the service first)
  -s, --status               read-only status
  -q, --stop                 stop the daemon (service kept)
      --rehearsal            dry run, no effects
  -d[=MSG] / -n              DELEGATION on (custom message) / off
  -t <name>                  watch only that herdr agent ("null" = all)
  -a / -o                    resume/delegate ALL claude agents / only the pin
  -l, --list                 list alive claude panels
  -p, --percent [PCT]        print the usage % (or set the threshold)
      --blocked              0 or the blocked target(s), for scripts
      --install / --uninstall  manage the systemd user service
```

## Configuration

Flags write `~/.config/climax/config.toml` (hot-reloaded by the daemon).
Use `null` to clear any optional value.

| Flag | Config key | Default |
| --- | --- | --- |
| `-d[=MSG]` / `-n` | `delegation` (`delegation_prompt`) | `false` |
| `-t <name>` | `herdr_agent_target` | all `claude` agents |
| `-a` / `-o` | `resume_all` | `true` |
| `--poll <secs>` | `poll_interval_secs` | `10` |
| `--margin <secs>` | `safety_margin_secs` | `15` |
| `--warning <secs>` | `warning_lead_time_secs` | `300` |
| `-p <pct>` / `--threshold` | `threshold_pct` | `90` |
| `--forced-reset <epoch>` | `forced_resets_at` | — |
| `--herdr <bin>` | `herdr_bin` | `herdr` |
| `--session <name>` | `herdr_session` | — |
| `--kind <kind>` | `herdr_agent_kind` | `claude` |
| `-r <text>` | `resume_message` | `continue` |
| `--no-install-hook` | `install_statusline_hook` | `true` |
| `--state-file <path>` | `state_path` | `~/.local/state/climax/state.json` |
| `--statusline <path>` | `statusline_json_path` | `~/.claude/statusline-cache.json` |
| `--settings <path>` | `claude_settings_path` | `~/.claude/settings.json` |

Queries for scripts and other apps (no config written): `-p` prints the
usage %, `-t` the wake targets, `-l`/`--list` the alive panels, and
`--blocked` prints `0` or the blocked target(s).

## How it works

1. Registers the `statusLine` hook in Claude Code's `settings.json`.
2. On every render, reads the quota/window from the hook's JSON payload.
3. Before the block it warns (and, with delegation on, hands the work over).
4. At `resets_at + margin` it resumes every alive agent (once per window).

## Uninstall

```bash
climax --uninstall  # removes only the service; hook, config and binary kept
rm ~/.local/bin/climax
```

## License

MIT
