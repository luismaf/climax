# climax

> Every great session deserves a proper climax.

`climax` is a quota guard for **Claude Code** sessions orchestrated over
[herdr](https://github.com/softarc-herdr/herdr) — no tmux, no UI scraping.
It watches your rate-limit window through the official JSON `statusLine`
hook, tells you when the block is coming, and when the window opens again
it wakes your agent(s) so the work doesn't wait for you.

Some people hit the 5-hour wall and stop. You? You just wait for the
second act. The best part is always the *climax*.

---

## Why

- **No scraping, no brittle hacks.** It reads the same JSON payload Claude
  Code feeds its own statusline. If Claude sees it, climax sees it.
- **Knows when to warn.** Before the hard limit (default: 5 minutes), it
  injects a heads-up so the current task gets wrapped up cleanly.
- **Wakes everyone when it frees up.** At the reset it sends a resume to
  every alive `claude`-kind agent in your herdr — working ones are never
  interrupted.
- **The star: DELEGATION.** Right before its turn ends, the main agent
  gathers the state of its work and **sends it to the other agents** on
  the same herdr, so they keep going with full context. Off by default;
  one flag turns it on.

## Install

**One line, any OS:**

```bash
sh -c "$(curl -fsSL https://raw.githubusercontent.com/luismaf/climax/master/scripts/install.sh)"
```

It detects your system and does the right thing:

| System | What it does |
| --- | --- |
| Ubuntu / Debian | installs the `.deb` via `apt` |
| Arch | builds the PKGBUILD with `makepkg` (the yay way, without needing the AUR) |
| macOS | puts the release binary in `~/.local/bin` |
| Windows (git-bash) | `cargo install --git` (needs [rustup](https://rustup.rs)) |
| any other Linux | release binary in `~/.local/bin` |

Whatever the method, it never adds a service or touches your config — the
systemd service (boot autorun) is always opt-in with `climax --install`.

**Ubuntu / Debian:** grab `climax_<version>_amd64.deb` (or `arm64`) from
the [releases page](https://github.com/luismaf/climax/releases):

```bash
sudo apt install ./climax_0.4.1_amd64.deb
```

**From source:**

```bash
cargo install --git https://github.com/luismaf/climax
```

**Arch:** the AUR package is on its way; meanwhile any Arch user can build
straight from this repo:

```bash
git clone https://github.com/luismaf/climax && cd climax/packaging/aur
makepkg -si
```

## Quick start

```bash
climax --install    # systemd user service, boot autorun (explicit, always)
climax -s           # the whole picture at a glance
climax -d           # turn DELEGATION on (the star)
```

Running the daemon never installs anything but the Claude Code hook.
The service is always your call.

## Usage

```
MODES (mutually exclusive; no flags = daemon):

  (no flags)        Daemon: watches your quota 24/7, warns before the
                    block, and auto-resumes the agent(s) at the reset.
  -d, --delegate    Turn the DELEGATION on (writes the config file).
  -n, --no-delegate Turn the DELEGATION off (default).
  -t, --target      Watch ONLY that herdr agent/pane ("null" = all).
  -c, --config      Path to the TOML config (default: ~/.config/climax/config.toml).
  -s, --status      Show current state (read-only).
  -x, --dry-run     Rehearsal without touching herdr or sending prompts.
```

Examples:

```bash
climax                    # daemon (what the service runs)
climax -s                 # quota + delegation + hook + agent states
climax -d                 # DELEGATION on: hand over the work before the wall
climax -t w5:p2           # watch only that agent
climax -t null            # back to watching every claude agent
climax -x                 # rehearsal, no side effects
```

## Configuration

Every setting is a flag that writes the TOML at `~/.config/climax/config.toml`
(the daemon hot-reloads it on change):

| Flag | Config key | Default |
| --- | --- | --- |
| `-d` / `-n` | `delegation` | `false` |
| `-t <name>` | `herdr_agent_target` | all `claude`-kind agents |
| `--poll <secs>` | `poll_interval_secs` | `10` |
| `--margin <secs>` | `safety_margin_secs` | `15` |
| `--warning <secs>` | `warning_lead_time_secs` | `300` |
| `--threshold <pct>` | `threshold_pct` | `85` |
| `--forced-reset <epoch>` | `forced_resets_at` | — |
| `--herdr <bin>` | `herdr_bin` | `herdr` from PATH |
| `--session <name>` | `herdr_session` | — |
| `--kind <kind>` | `herdr_agent_kind` | `claude` |
| `--resume-msg <text>` | `resume_message` | `continue` |
| `--prompt <text>` | `delegation_prompt` | the embedded one |
| `--no-install-hook` | `install_statusline_hook` | `true` |
| `--state-file <path>` | `state_path` | `~/.local/state/climax/state.json` |
| `--statusline <path>` | `statusline_json_path` | `~/.claude/statusline-cache.json` |
| `--settings <path>` | `claude_settings_path` | `~/.claude/settings.json` |

Use `null` to clear any optional value: `climax -t null`.

## How it works

1. On start it registers the `statusLine` hook in your Claude Code
   `settings.json` (unless you opt out).
2. Claude Code feeds the hook a JSON payload on every render; climax
   persists it and derives `used_pct`, `hard_limit` and `resets_at`.
3. Approaching the window's end (and with delegation on), the main agent
   is asked to collect its work state and send it to the other agents in
   the herdr.
4. At `resets_at + safety_margin` it sends a resume to every target agent
   and marks the window as done, so each reset triggers exactly one wake.

Force a different window (stale hook, several agents, or simulation) with
`climax --forced-reset 1786292400`; come back to the hook's timing with
`climax --forced-reset null`.

## Uninstall

```bash
climax --uninstall     # removes the service (the binary stays; the hook keeps working)
rm ~/.local/bin/climax # remove the binary
```

## License

MIT — do whatever keeps you at a steady pace.
