use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, Utc};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

// ─────────────────────────────────────────────
// Colors (only when stdout is a terminal)
// ─────────────────────────────────────────────
const GREEN: &str = "\x1b[0;32m";
const YELLOW: &str = "\x1b[0;33m";
const RED: &str = "\x1b[0;31m";
const CYAN: &str = "\x1b[0;36m";
const RESET: &str = "\x1b[0m";

fn painted(s: &str, code: &str) -> String {
    if io::stdout().is_terminal() {
        format!("{code}{s}{RESET}")
    } else {
        s.to_string()
    }
}

// ─────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    /// Porcentaje de fallback (si no hay resets_at)
    #[serde(default = "default_threshold")]
    threshold_pct: f64,
    /// Segundos antes del bloqueo para inyectar el aviso (default 300s = 5 min)
    #[serde(default = "default_warning_lead")]
    warning_lead_time_secs: u64,
    /// Extra seconds after resets_at before sending the resume.
    /// Kept deliberately small: you want to wake up as soon as the quota
    /// frees up (the reset usually lands on time; 15s covers clock skew).
    #[serde(default = "default_margin")]
    safety_margin_secs: u64,
    /// Force the reset window (epoch, seconds), ignoring the hook JSON one.
    /// Useful if you have several agents with different windows, a stale
    /// hook, or you want to align the wake-up to a specific reset.
    /// 'null' (default) = use the hook's.
    #[serde(default)]
    forced_resets_at: Option<i64>,
    /// How often (seconds) the JSON is checked. Clamped at runtime to a
    /// sane minimum so it never hammers the CPU or herdr's socket.
    #[serde(default = "default_poll")]
    poll_interval_secs: u64,

    /// herdr binary to invoke (in case it isn't on PATH with that exact name).
    #[serde(default = "default_herdr_bin")]
    herdr_bin: String,
    /// Named herdr session, if any (passed as HERDR_SESSION).
    /// Leave as None to use the default session.
    herdr_session: Option<String>,
    /// The agent kind we look for with `herdr agent list` when there is
    /// no explicit target (see herdr_agent_target). See `herdr agent start --help`
    /// for the supported kinds; ours is "claude".
    #[serde(default = "default_herdr_agent_kind")]
    herdr_agent_kind: String,
    /// Alive agent name or explicit pane_id (e.g. "w1:p1" or the name you
    /// gave it with `herdr agent start <name> ...` / `agent rename`).
    /// If set, NO autodetection is done — it is used as-is.
    /// Needed if you run more than one Claude agent at once.
    herdr_agent_target: Option<String>,

    /// Exact delegation prompt text. If not in the config, the embedded
    /// default is used (DEFAULT_DELEGATION_PROMPT).
    #[serde(default = "default_delegation_prompt")]
    delegation_prompt: String,
    /// If false (default), DELEGATION is off: nothing is injected into the
    /// agent before the block (only auto-resume at the reset).
    /// Turn it on with `--set delegation=true`: before the hard limit the
    /// `delegation_prompt` is injected into the main agent (see the
    /// default embedded in DEFAULT_DELEGATION_PROMPT).
    #[serde(default)]
    delegation: bool,
    #[serde(default = "default_resume_msg")]
    resume_message: String,
    #[serde(default = "default_state_path")]
    state_path: PathBuf,
    #[serde(default = "default_statusline_path")]
    statusline_json_path: PathBuf,

    /// Si true (default), al arrancar el daemon verifica que el settings.json
    /// de Claude Code tenga el statusLine hook de climax y lo instala si
    /// falta. Nunca pisa un statusLine configurado con otro comando.
    #[serde(default = "default_true")]
    install_statusline_hook: bool,
    /// Ruta del settings.json de usuario de Claude Code. Default:
    /// $CLAUDE_CONFIG_DIR/settings.json o ~/.claude/settings.json.
    claude_settings_path: Option<PathBuf>,
}

fn default_threshold() -> f64 {
    85.0
}
fn default_warning_lead() -> u64 {
    300
}
fn default_margin() -> u64 {
    15
}
fn default_poll() -> u64 {
    10
}
fn default_herdr_bin() -> String {
    "herdr".to_string()
}
fn default_herdr_agent_kind() -> String {
    "claude".to_string()
}
fn default_delegation_prompt() -> String {
    DEFAULT_DELEGATION_PROMPT.to_string()
}
fn default_resume_msg() -> String {
    "continue".into()
}
fn default_state_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".local/state/climax/state.json")
}
fn default_true() -> bool {
    true
}
fn default_statusline_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".claude/statusline-cache.json")
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold_pct: default_threshold(),
            warning_lead_time_secs: default_warning_lead(),
            safety_margin_secs: default_margin(),
            forced_resets_at: None,
            poll_interval_secs: default_poll(),
            herdr_bin: default_herdr_bin(),
            herdr_session: None,
            herdr_agent_kind: default_herdr_agent_kind(),
            herdr_agent_target: None,
            delegation_prompt: default_delegation_prompt(),
            delegation: false,
            resume_message: default_resume_msg(),
            state_path: default_state_path(),
            statusline_json_path: default_statusline_path(),
            install_statusline_hook: default_true(),
            claude_settings_path: None,
        }
    }
}

const DEFAULT_DELEGATION_PROMPT: &str = r#"URGENT — YOUR RATE-LIMIT WINDOW IS ABOUT TO EXPIRE. This Claude Code session is about to hit the hard block (the window comes from your plan/usage — not always 5 hours — and it won't recover until it resets). You have ONE job: maximize the autonomous work of the other agents without you. NO implementing, NO chatting, NO asking permission, NO tokens spent describing what you do.
MANDATORY PLAN (do it NOW, before the block):
1. Prepare your session: stop any long-running task and guarantee your session is left easy to resume (check the working tree; no half-made changes without a commit).
2. Write a work plan of at least 200 hours (or the maximum that makes sense for the current project) in a single file:
   - PWD/<PROJECT>-delegation-plan.md
   - Clear tasks, prioritized by value and independence, each with a verifiable "done" criterion, in the exact order a fresh agent should execute them.
   - Include at the end a "CONTEXT" block: where the code lives, repo conventions, how to run build/tests, and the current state of the work.
3. Delegate ALL of that with herdr (you have HERDR_ENV=1 and the skill):
   - herdr pane split --current --direction right --cwd "$PWD" --no-focus
     → capture the pane-id that the output returns.
   - herdr agent start opc-deleg --kind opencode --pane <pane-id> --timeout 300000
     → wait for success (agent ready, detected in the pane).
   - herdr agent prompt opc-deleg "Work autonomously. Read <plan> and execute it. Done criteria are in the plan. Update the state file (below) at the end of each task. If you hit a blocker that needs a business decision, resolve it the best reasonable way and keep going, documenting the decision." --wait
     → confirm that the prompt was taken (don't assume it).
   - If there are two or more fully independent areas, repeat the split/start/prompt per area (max 3 agents). NEVER split one task between two agents.
4. If the herdr skill is not available, delegate through whatever fallback you have configured (opencode another route, subagent, coding MCP). Don't stop delegating.
5. Update HANDOFF.md (or the root-project equivalent):
   - How to resume your own work when you return (what you did, where you left it, what you tried).
   - What was delegated and to whom (agent name + pane-id), with which plan, and the state of each area.
   - What remains when the quota comes back.
6. When everything is delegated and confirmed: wait calmly for the block. Don't keep consuming tokens; don't redo delegated work; don't write long summaries. A short line is enough.
When the reset comes (the guard handles it automatically), the first action will be to resume from HANDOFF.md, respecting that the delegated work stays with the other agent."#;

// ─────────────────────────────────────────────
// State
// ─────────────────────────────────────────────
#[derive(Debug, Default, Serialize, Deserialize)]
struct GuardState {
    last_injected_reset_at: Option<i64>,
    last_hard_limit_reset_at: Option<i64>,
    /// Window for which we already warned that resets_at went stale
    /// (avoids repeating the warning every poll).
    warned_stale_reset_at: Option<i64>,
    /// Woken per agent: target -> resets_at of the last window for which
    /// the resume was already sent (multi-target dedup).
    #[serde(default)]
    woken_targets: HashMap<String, i64>,
}

// ─────────────────────────────────────────────
// Rate Info (viene del JSON del hook, no de la pantalla)
// ─────────────────────────────────────────────
#[derive(Debug, Clone, Default)]
struct RateInfo {
    used_pct: f64,
    resets_at: Option<i64>,
    hard_limit_hit: bool,
}

// ─────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────
const AFTER_HELP: &str = r#"MODES (mutually exclusive; no flags = daemon):

  (no flags)        Daemon: watches your quota 24/7, warns before the block,
                    and auto-resumes the agent(s) when the window resets.
                    If several alive kind='claude' agents exist in herdr,
                    ALL of them are resumed (working ones are not touched).
  --status          Show current state (read-only, doesn't touch anything).
  --dry-run         Full daemon rehearsal WITHOUT running herdr or sending
                    prompts (only simulates; state.json IS still saved).
  --write-statusline  statusLine hook command for Claude Code: reads the
                    JSON payload Claude sends via stdin and persists it in
                    statusline_json_path. Invoked by Claude Code on every
                    render; not meant to be run by hand.
  --install-service  Install the systemd user service with boot autorun
                    (copies the binary to ~/.local/bin) and start it.
--uninstall-service  Uninstall the service (the binary stays, so the
                    statusline hook keeps working).
  -d, --delegate    Turn the DELEGATION on (writes the config file).
  -n, --no-delegate Turn the DELEGATION off (default).
  -t, --target      Watch ONLY that herdr agent/pane ("null" = all).
  -c, --config      Path to the TOML config (default: ~/.config/climax/config.toml).
  -s, --status      Show current state (read-only, doesn't touch anything).
  -x, --dry-run     Full daemon rehearsal WITHOUT running herdr or sending
                    prompts (only simulates; state.json IS still saved).

DELEGATION — the star (OFF by default):

   Before Claude's turn ends (the rate-limit window is about to run out,
   per warning_lead_time_secs, default 300s), climax injects into the
   main agent a prompt asking it to:
     - gather the state of its work (what it did, where it left off,
       what it tried), and
     - delegate it: SEND that info to other agent(s) on the same herdr,
       so the work keeps moving without you and without losing context.

   It's optional and coexists with auto-resume (which keeps working):

     climax -d
     climax --prompt 'your own prompt'   (customize the delegation text)
     climax -n                          (turn it back off)

CONFIGURATION (write the TOML with these flags; the daemon hot-reloads):

  -d, --delegate            delegation = true    (the star, explicitly)
  -n, --no-delegate         delegation = false   (default)
  -t, --target <name>       herdr_agent_target   ("null" → watch ALL
                            kind='claude' agents; useful to disambiguate).
      --poll <secs>         poll_interval_secs   (min 5, default 10).
      --margin <secs>       safety_margin_secs   (default 15, post-reset).
      --warning <secs>      warning_lead_time_secs (default 300).
      --threshold <pct>     threshold_pct (85; only when no resets_at).
      --forced-reset <epoch>  Force the reset window ("null" clears).
      --herdr <bin>         herdr binary (default: PATH / ~/.local/bin).
      --session <name>      herdr session ("null" clears).
      --kind <kind>         herdr_agent_kind (default: claude).
      --resume-msg <text>   resume_message (default: continue).
      --prompt <text>       delegation_prompt (custom delegation text).
      --no-install-hook     don't auto-install the statusLine hook.
      --state-file <path>   state_path (daemon state, JSON).
      --statusline <path>   statusline_json_path (hook cache).
      --settings <path>     claude_settings_path ("null" = default).

EXAMPLES:
  climax                                 Daemon (what the service runs)
  climax -d                              Turn the star on (delegation)
  climax -t null                          Watch ALL claude agents
  climax -t w5:p2                        Watch only that agent
  climax -x                              Rehearsal without touching agents
  climax -s                              Status (quota + agents)
  climax -c /tmp/my-config.toml -s       Different config
  climax --install-service               Install + start the service

NOTES:
  - Instead of flags you can edit ~/.config/climax/config.toml by hand
    (same values; the daemon hot-reloads the file).
  - Service logs: journalctl --user -u climax.service -f
  - Install with a package manager: Arch (AUR): yay -S climax · Ubuntu/Debian: .deb from the release.
"#;

#[derive(Parser, Debug)]
#[command(
    name = "climax",
    version,
    about = "Claude Code quota guard (JSON hook + auto-resume, orchestrated over herdr)",
    after_help = AFTER_HELP
)]
struct Cli {
    /// Path to the TOML config file (default: ~/.config/climax/config.toml).
    #[arg(short, long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Show status: % used, reset, window, agents (read-only).
    #[arg(short = 's', long)]
    status: bool,

    /// Rehearsal: full daemon cycle WITHOUT running herdr or sending prompts.
    #[arg(short = 'x', long)]
    dry_run: bool,

    /// Turn DELEGATION on (writes to the config file, hot-reloaded).
    #[arg(short = 'd', long)]
    delegate: bool,

    /// Turn DELEGATION off (default; writes to the config file).
    #[arg(short = 'n', long)]
    no_delegate: bool,

    /// Watch ONLY that agent/pane of herdr. "null" clears the pin and
    /// goes back to watching ALL kind='claude' agents.
    #[arg(short = 't', long, value_name = "AGENT")]
    target: Option<String>,

    /// statusLine hook for Claude Code: receives JSON on stdin and stores
    /// it in statusline_json_path (the guard reads it afterwards).
    #[arg(long)]
    write_statusline: bool,

    /// Install the systemd user service (boot autorun) and start it.
    #[arg(long)]
    install_service: bool,

    /// Uninstall the systemd service (keeps the binary for the hook).
    #[arg(long)]
    uninstall_service: bool,

    /// Check frequency in seconds (minimum 5; default 10).
    #[arg(long, value_name = "SECS")]
    poll: Option<u64>,

    /// Seconds after the reset before sending the resume (default 15).
    #[arg(long, value_name = "SECS")]
    margin: Option<u64>,

    /// How many seconds before the block to warn (default 300).
    #[arg(long, value_name = "SECS")]
    warning: Option<u64>,

    /// Fallback % of usage that triggers the warning when there is no
    /// resets_at in the hook JSON (default 85).
    #[arg(long, value_name = "PCT")]
    threshold: Option<f64>,

    /// Force the reset window (epoch seconds). "null" clears it.
    #[arg(long, value_name = "EPOCH")]
    forced_reset: Option<String>,

    /// herdr binary (path or name) to invoke.
    #[arg(long, value_name = "BIN")]
    herdr: Option<String>,

    /// Named herdr session (passed as HERDR_SESSION). "null" removes it.
    #[arg(long, value_name = "NAME")]
    session: Option<String>,

    /// herdr agent kind to watch (default: claude).
    #[arg(long, value_name = "KIND")]
    kind: Option<String>,

    /// Resume text sent when the window opens (default: continue).
    #[arg(long, value_name = "TEXT")]
    resume_msg: Option<String>,

    /// Custom delegation prompt (default: the embedded one).
    #[arg(long, value_name = "TEXT")]
    prompt: Option<String>,

    /// Don't auto-install the statusLine hook on daemon start.
    #[arg(long)]
    no_install_hook: bool,

    /// Path of the guard state file (default: ~/.local/state/climax/state.json).
    #[arg(long, value_name = "PATH")]
    state_file: Option<PathBuf>,

    /// Path of the statusline cache written by the hook.
    #[arg(long, value_name = "PATH")]
    statusline: Option<PathBuf>,

    /// Path of the Claude Code settings.json. "null" restores the default.
    #[arg(long, value_name = "PATH")]
    settings: Option<String>,
}

// ─────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    if cli.install_service || cli.uninstall_service {
        if cli.install_service && cli.uninstall_service {
            bail!("--install-service and --uninstall-service are mutually exclusive");
        }
        if cli.status || cli.dry_run || cli.write_statusline {
            bail!("--install-service/--uninstall-service don't combine with other modes");
        }
        return if cli.install_service {
            install_service()
        } else {
            uninstall_service()
        };
    }

    let config_path: PathBuf = cli.config.clone().unwrap_or_else(default_config_path);

    // Flags directos de configuración (escriben en el TOML, hot-reload).
    let settings = collect_settings(&cli)?;
    if !settings.is_empty() {
        if cli.status || cli.dry_run || cli.write_statusline {
            bail!("config flags don't combine with --status/--dry-run/--write-statusline");
        }
        apply_config_settings(&config_path, &settings)?;
        return Ok(());
    }

    let mut config = load_config_from(&config_path)?;
    let dry_run = cli.dry_run;

    if cli.write_statusline {
        return write_statusline(&config);
    }

    // Polling interval hard floor: never under 5s, whatever the config says.
    clamp_poll(&mut config);

    if cli.status {
        let info = gather_rate_info(&config)?;
        print_status(&info, &config);
        println!(
            "delegation     : {}",
            if config.delegation {
                painted("enabled", GREEN)
            } else {
                painted("disabled (default)", RED)
            }
        );
        if let Some(forced) = config.forced_resets_at {
            println!(
                "resets_forced  : {} (forced window; ignores the hook resets_at)",
                forced
            );
        }
        println!(
            "statusline_hook: {}",
            match hook_state(&claude_settings_path(&config)) {
                HookState::Present => painted("installed", GREEN),
                HookState::NoSettings => painted("no settings.json", YELLOW),
                HookState::Missing => painted("missing (auto-install on daemon start)", YELLOW),
                HookState::Other => painted("points to another command", RED),
                HookState::Invalid => painted("invalid JSON", RED),
            }
        );
        match resolve_targets(&config).await {
            Ok(targets) => {
                println!("herdr_targets  : {}", targets.join(", "));
                for t in &targets {
                    match get_agent_status(&config, t).await {
                        Ok(status) => {
                            let code = match status.as_str() {
                                "working" | "busy" => GREEN,
                                "idle" | "free" | "waiting" => YELLOW,
                                "queued" | "pending" => CYAN,
                                _ => RED,
                            };
                            println!("  {t} : {}", painted(&status, code));
                        }
                        Err(e) => println!("  {t} : {}", painted(&format!("(error: {e:#})"), RED)),
                    }
                }
            }
            Err(e) => println!(
                "herdr_targets  : {}",
                painted(&format!("(not resolved: {e:#})"), RED)
            ),
        }
        return Ok(());
    }

    info!("climax started (JSON hook + herdr, no UI scraping)");
    info!(
        "warning_lead = {}s | poll = {}s | margin = {}s | agent_kind = {}",
        config.warning_lead_time_secs,
        config.poll_interval_secs,
        config.safety_margin_secs,
        config.herdr_agent_kind
    );

    if let Err(e) = run_hook_install(&config, dry_run) {
        warn!("Hook check/install: {:#}", e);
    }

    if !resolve_path(&config.statusline_json_path).exists() {
        warn!(
            "{} does not exist yet: the hook will generate it with the first \
             Claude Code render.",
            config.statusline_json_path.display()
        );
    }

    let mut state = load_state(&config.state_path)?;
    let mut cached_target: Option<String> = None;
    let mut last_config_mtime = file_mtime(&config_path);

    loop {
        // Hot-reload: if the config file changed (via --set or by hand),
        // reload it on the next cycle without restarting the service.
        if let Some(mtime) = file_mtime(&config_path) {
            if Some(mtime) != last_config_mtime {
                last_config_mtime = Some(mtime);
                match load_config_from(&config_path) {
                    Ok(new_config) => {
                        config = new_config;
                        clamp_poll(&mut config);
                        cached_target = None;
                        info!(
                            "Config hot-reloaded from {} (poll={}s, delegation={})",
                            config_path.display(),
                            config.poll_interval_secs,
                            config.delegation
                        );
                    }
                    Err(e) => warn!("Could not reload config: {:#}", e),
                }
            }
        }

        match run_once(&config, &mut state, &mut cached_target, dry_run).await {
            Ok(Action::Continue) => {}
            Ok(Action::SleepUntil(reset_at)) => {
                let wait = wait_duration(reset_at, config.safety_margin_secs);
                info!(
                    "Hard limit detected. Sleeping until reset + margin (~{}s)...",
                    wait.as_secs()
                );
                sleep(wait).await;

                state.last_hard_limit_reset_at = Some(reset_at);
                save_state(&config.state_path, &state)?;
                match resume_targets(&config, &mut state, &mut cached_target, reset_at, dry_run)
                    .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        // Los targets que no se anotaron se reintentan en el
                        // next cycle (run_once detects pending targets).
                        warn!("Multi-target resume incomplete: {:#}", e);
                    }
                }
            }
            Err(e) => warn!("Error in cycle: {:#}", e),
        }
        sleep(Duration::from_secs(config.poll_interval_secs)).await;
    }
}

enum Action {
    Continue,
    SleepUntil(i64),
}

async fn run_once(
    config: &Config,
    state: &mut GuardState,
    cached_target: &mut Option<String>,
    dry_run: bool,
) -> Result<Action> {
    let info = gather_rate_info(config)?;
    let now = Utc::now().timestamp();
    debug!(
        "used={:.1}% | resets_at={:?} | hard={}",
        info.used_pct, info.resets_at, info.hard_limit_hit
    );

    // 1. Hard limit: dormir y resumir (todos los targets al reset)
    if info.hard_limit_hit {
        if let Some(reset_at) = info.resets_at {
            if state.last_hard_limit_reset_at == Some(reset_at) {
                // Ventana ya procesada: si quedaron targets sin destrabar
                // (a send failed), retry on the next cycle.
                match resolve_targets(config).await {
                    Ok(targets) => {
                        let pending = targets
                            .iter()
                            .any(|t| state.woken_targets.get(t) != Some(&reset_at));
                        if pending {
                            debug!("Resume still pending for targets of this window");
                            return Ok(Action::SleepUntil(reset_at));
                        }
                    }
                    Err(e) => {
                        warn!("Could not list targets to retry resume: {:#}", e)
                    }
                }

                // If way past the reset and resets_at stayed the same,
                // the hook JSON is stale (or the window moved):
                // warn ONCE per window and keep polling.
                if now - reset_at > (config.safety_margin_secs as i64) + 60
                    && state.warned_stale_reset_at != Some(reset_at)
                {
                    state.warned_stale_reset_at = Some(reset_at);
                    save_state(&config.state_path, state)?;
                    warn!(
                        "Resume already sent for reset_at={} but the limit is still \
                         active: the hook resets_at is stale or the window moved. \
                         The next poll will pick up the new resets_at and retry.",
                        reset_at
                    );
                }
                return Ok(Action::Continue);
            }
            return Ok(Action::SleepUntil(reset_at));
        }
    }

    // 2. Aviso "unos minutos antes" (warning_lead_time_secs)
    let should_inject = if let Some(reset_at) = info.resets_at {
        let remaining = reset_at - now;
        remaining > 0 && remaining <= (config.warning_lead_time_secs as i64) && info.used_pct < 99.9
    } else {
        info.used_pct >= config.threshold_pct
    };

    if should_inject {
        let reset_at = info
            .resets_at
            .unwrap_or(now + config.warning_lead_time_secs as i64);

        if let Some(last) = state.last_injected_reset_at {
            if (reset_at - last).abs() <= 120 {
                debug!("Already injected for this window, skip");
                return Ok(Action::Continue);
            }
        }

        if !config.delegation {
            info!(
                "Delegation disabled (delegation=false): skipping the \
                 delegation prompt (auto-resume stays active)"
            );
            state.last_injected_reset_at = Some(reset_at);
            save_state(&config.state_path, state)?;
            return Ok(Action::Continue);
        }

        info!(
            "Block imminent! (remaining ~{}s) - injecting urgent delegation prompt",
            reset_at - now
        );

        if dry_run {
            info!("[dry-run] would inject the delegation prompt");
        } else {
            let target = ensure_target(config, cached_target)
                .await
                .context("resolving herdr agent to inject the notice")?;
            if let Err(e) =
                send_to_herdr(config, &target, &config.delegation_prompt, None, None).await
            {
                *cached_target = None;
                return Err(e).context("delegation prompt injection via herdr failed");
            }
            info!("Delegation prompt sent to '{}'", target);
        }
        state.last_injected_reset_at = Some(reset_at);
        save_state(&config.state_path, state)?;
    }

    Ok(Action::Continue)
}

// ─────────────────────────────────────────────
// Lectura del JSON (Statusline Hook) — 100% independiente del pane manager
// ─────────────────────────────────────────────
fn gather_rate_info(config: &Config) -> Result<RateInfo> {
    let path = resolve_path(&config.statusline_json_path);
    if !path.exists() {
        debug!("statusline JSON does not exist yet: {:?}", path);
        return Ok(RateInfo::default());
    }
    let data = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let v: Value = serde_json::from_str(&data).context("parsing statusline JSON")?;

    let used = v
        .pointer("/rate_limits/five_hour/used_percentage")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let resets_raw = v
        .pointer("/rate_limits/five_hour/resets_at")
        .and_then(|x| x.as_i64())
        .or_else(|| {
            v.pointer("/rate_limits/five_hour/reset_at")
                .and_then(|x| x.as_i64())
        });

    // Claude a veces manda ms en vez de s. Si hay forced_resets_at en la
    // config, ese gana siempre (ventana forzada por el usuario).
    let resets_at = config.forced_resets_at.or_else(|| {
        resets_raw.map(|ts| {
            if ts > 1_000_000_000_000 {
                ts / 1000
            } else {
                ts
            }
        })
    });
    let now = Utc::now().timestamp();
    let hard_limit_hit = used >= 99.9 || resets_at.is_some_and(|r| now >= r);

    Ok(RateInfo {
        used_pct: used,
        resets_at,
        hard_limit_hit,
    })
}

// ─────────────────────────────────────────────
// herdr: discovery and sending (replaces the whole tmux block)
// ─────────────────────────────────────────────

#[derive(Debug, Clone)]
struct HerdrAgentEntry {
    target: String, // name if present, else pane_id
    kind: Option<String>,
}

/// Resuelve el binario de herdr a invocar: si `herdr_bin` es una ruta
/// (absoluta o con /), se usa tal cual. Si es solo un nombre, se busca
/// primero en ~/.local/bin (los servicios systemd de usuario corren con
/// a minimal PATH and can't see $HOME bins) and then falls back to the process
/// PATH. Returns path-or-name, always usable by Command::new.
fn herdr_bin_resolved(config: &Config) -> PathBuf {
    let bin = Path::new(&config.herdr_bin);
    if bin.components().count() > 1 {
        return bin.to_path_buf();
    }
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".local/bin").join(&config.herdr_bin);
        if candidate.exists() {
            return candidate;
        }
    }
    bin.to_path_buf()
}

fn herdr_command(config: &Config) -> Command {
    let mut cmd = Command::new(herdr_bin_resolved(config));
    if let Some(session) = &config.herdr_session {
        cmd.env("HERDR_SESSION", session);
    }
    cmd
}

fn herdr_error_message(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(sin salida de error)".to_string();
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        let err = v.get("error").cloned().unwrap_or(v);
        let code = err
            .get("code")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        let message = err
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or(trimmed);
        return format!("[{code}] {message}");
    }
    trimmed.to_string()
}

fn parse_agent_list(v: &Value) -> Vec<HerdrAgentEntry> {
    let arr = v
        .pointer("/result/agents")
        .or_else(|| v.pointer("/agents"))
        .and_then(|x| x.as_array())
        .cloned()
        .or_else(|| v.pointer("/result").and_then(|x| x.as_array()).cloned())
        .or_else(|| v.as_array().cloned())
        .unwrap_or_default();

    arr.iter()
        .filter_map(|item| {
            let pane_id = item
                .get("pane_id")
                .and_then(|x| x.as_str())
                .or_else(|| item.get("id").and_then(|x| x.as_str()))?
                .to_string();
            let name = item
                .get("name")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            let kind = item
                .get("kind")
                .and_then(|x| x.as_str())
                .or_else(|| item.get("agent").and_then(|x| x.as_str()))
                .or_else(|| item.get("agent_kind").and_then(|x| x.as_str()))
                .map(str::to_string);
            let target = name.unwrap_or(pane_id);
            Some(HerdrAgentEntry { target, kind })
        })
        .collect()
}

/// Resolves the targets of `herdr agent ...` (unique names or pane_ids).
/// If `herdr_agent_target` is set in the config, only that one is used.
/// ese). Si no, se listan con `herdr agent list` y se filtra por
/// `herdr_agent_kind`: TODOS los que matcheen son targets (multi-agente:
/// the reset wakes all; the delegation notice goes to the first one).
/// Zero matches is an explicit error, not a guess.
async fn resolve_targets(config: &Config) -> Result<Vec<String>> {
    if let Some(explicit) = &config.herdr_agent_target {
        return Ok(vec![explicit.clone()]);
    }

    let output = herdr_command(config)
        .args(["agent", "list"])
        .output()
        .await
        .context("running 'herdr agent list' (is herdr in PATH and the server up?)")?;

    if !output.status.success() {
        bail!(
            "'herdr agent list' failed: {}",
            herdr_error_message(&output.stderr)
        );
    }

    let v: Value =
        serde_json::from_slice(&output.stdout).context("parsing 'herdr agent list' JSON")?;
    let agents = parse_agent_list(&v);

    let matches: Vec<String> = agents
        .iter()
        .filter(|a| a.kind.as_deref() == Some(config.herdr_agent_kind.as_str()))
        .map(|a| a.target.clone())
        .collect();

    match matches.as_slice() {
        [] => bail!(
            "No kind='{}' agent found alive in herdr. Seen: {:?}. \
             Set climax's herdr_agent_target if the detected kind is off.",
            config.herdr_agent_kind,
            agents
                .iter()
                .map(|a| format!("{}({})", a.target, a.kind.as_deref().unwrap_or("?")))
                .collect::<Vec<_>>()
        ),
        // Ojo: devolvemos TODOS los que matchean (multi-target), no solo uno.
        all => Ok(all.to_vec()),
    }
}

async fn ensure_target(config: &Config, cached: &mut Option<String>) -> Result<String> {
    if let Some(t) = cached {
        return Ok(t.clone());
    }
    let targets = resolve_targets(config).await?;
    let t = targets[0].clone();
    info!("Resolved target agent: {}", t);
    *cached = Some(t.clone());
    Ok(t)
}

/// Despierta TODOS los targets de herdr cuando se abre la ventana.
/// - Agents already `working` are not touched (marked as ok).
/// - Los que ya fueron resumidos para este `reset_at` se omiten (dedup).
/// - If a send fails, it stays unmarked: the next cycle retries.
async fn resume_targets(
    config: &Config,
    state: &mut GuardState,
    cached_target: &mut Option<String>,
    reset_at: i64,
    dry_run: bool,
) -> Result<()> {
    let targets = resolve_targets(config).await?;
    info!(
        "Window {} opened: checking {} target(s)",
        reset_at,
        targets.len()
    );

    for t in &targets {
        if state.woken_targets.get(t) == Some(&reset_at) {
            debug!("'{}' already has resume for this window, skip", t);
            continue;
        }

        if !dry_run {
            // We don't stomp an agent that is already working: we check
            // chequeamos su estado real. Si no se puede consultar, se manda
            // anyway (safer to release than to leave stuck).
            match get_agent_status(config, t).await {
                Ok(s) if s == "working" => {
                    info!("'{}' is already working ({}) — not touching", t, s);
                    state.woken_targets.insert(t.clone(), reset_at);
                    continue;
                }
                Ok(s) => debug!("'{}' is '{}' → sending resume", t, s),
                Err(e) => debug!("Could not verify '{}' ({:#}) → sending anyway", t, e),
            }
        }

        let ok = if dry_run {
            info!(
                "[dry-run] would send resume to '{}': {}",
                t, config.resume_message
            );
            true
        } else {
            info!("Sending resume to '{}': {}", t, config.resume_message);
            match send_to_herdr(config, t, &config.resume_message, None, None).await {
                Ok(_) => true,
                Err(e) => {
                    error!("Error sending resume to '{}': {:#}", t, e);
                    *cached_target = None;
                    false
                }
            }
        };
        if ok {
            state.woken_targets.insert(t.clone(), reset_at);
        }
    }

    save_state(&config.state_path, state)?;
    Ok(())
}

/// Sends text + Enter atomically via `herdr agent prompt`.
/// Sin shell, sin buffers, sin C-c previo: `agent prompt` funciona
/// even while the agent is working.
async fn send_to_herdr(
    config: &Config,
    target: &str,
    text: &str,
    wait_until: Option<&[&str]>,
    timeout_ms: Option<u64>,
) -> Result<Value> {
    let mut cmd = herdr_command(config);
    cmd.args(["agent", "prompt", target, text]);
    if let Some(states) = wait_until {
        cmd.arg("--wait");
        for s in states {
            cmd.args(["--until", *s]);
        }
    }
    if let Some(ms) = timeout_ms {
        cmd.args(["--timeout", &ms.to_string()]);
    }

    let output = cmd
        .output()
        .await
        .with_context(|| format!("ejecutando 'herdr agent prompt {target}'"))?;

    if !output.status.success() {
        bail!(
            "'herdr agent prompt' to '{}' failed: {}",
            target,
            herdr_error_message(&output.stderr)
        );
    }

    Ok(serde_json::from_slice(&output.stdout).unwrap_or(Value::Null))
}

async fn get_agent_status(config: &Config, target: &str) -> Result<String> {
    let output = herdr_command(config)
        .args(["agent", "get", target])
        .output()
        .await
        .context("ejecutando 'herdr agent get'")?;

    if !output.status.success() {
        bail!("{}", herdr_error_message(&output.stderr));
    }

    let v: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    let status = v
        .pointer("/result/agent/agent_status")
        .or_else(|| v.pointer("/result/agent/status"))
        .or_else(|| v.pointer("/result/agent_status"))
        .or_else(|| v.pointer("/result/status"))
        .or_else(|| v.pointer("/status"))
        .or_else(|| v.pointer("/agent_status"))
        .and_then(|x| x.as_str())
        .map(str::to_string);

    match status {
        Some(s) => Ok(s),
        None => {
            let raw = serde_json::to_string(&v).unwrap_or_default();
            bail!(
                "no se pudo extraer el estado del agente; respuesta cruda: {}",
                raw.chars().take(300).collect::<String>()
            )
        }
    }
}

// ─────────────────────────────────────────────
// Helpers (config/estado, sin cambios de fondo)
// ─────────────────────────────────────────────
fn resolve_path(p: &Path) -> PathBuf {
    if let Some(s) = p.to_str() {
        if let Some(stripped) = s.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(stripped);
            }
        }
    }
    p.to_path_buf()
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_default()
        .join("climax/config.toml")
}

fn load_config_from(path: &Path) -> Result<Config> {
    if path.exists() {
        let content = fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    } else {
        warn!("No hay config en {:?}, usando defaults", path);
        Ok(Config::default())
    }
}

/// Piso duro para el intervalo de polling: nunca menos de 5s, sin
/// importar lo que diga el config. Evita martillar CPU/socket.
fn clamp_poll(config: &mut Config) {
    const MIN_POLL_SECS: u64 = 5;
    if config.poll_interval_secs < MIN_POLL_SECS {
        warn!(
            "poll_interval_secs={} es demasiado bajo, se ajusta a {}s",
            config.poll_interval_secs, MIN_POLL_SECS
        );
        config.poll_interval_secs = MIN_POLL_SECS;
    }
}

/// mtime of a file, or None if missing/failed.
fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Reúne los flags directos de configuración del CLI en una lista de
/// (clave, valor) tipados. `None` de valor = borrar la clave (null).
fn collect_settings(cli: &Cli) -> Result<Vec<(String, Option<toml::Value>)>> {
    let mut s: Vec<(String, Option<toml::Value>)> = Vec::new();
    if cli.delegate && cli.no_delegate {
        bail!("--delegate and --no-delegate are mutually exclusive");
    }
    if cli.delegate {
        s.push(("delegation".into(), Some(toml::Value::Boolean(true))));
    }
    if cli.no_delegate {
        s.push(("delegation".into(), Some(toml::Value::Boolean(false))));
    }
    if let Some(t) = &cli.target {
        if t == "null" {
            s.push(("herdr_agent_target".into(), None));
        } else {
            s.push((
                "herdr_agent_target".into(),
                Some(toml::Value::String(t.clone())),
            ));
        }
    }
    if let Some(v) = cli.poll {
        s.push((
            "poll_interval_secs".into(),
            Some(toml::Value::Integer(v as i64)),
        ));
    }
    if let Some(v) = cli.margin {
        s.push((
            "safety_margin_secs".into(),
            Some(toml::Value::Integer(v as i64)),
        ));
    }
    if let Some(v) = cli.warning {
        s.push((
            "warning_lead_time_secs".into(),
            Some(toml::Value::Integer(v as i64)),
        ));
    }
    if let Some(v) = cli.threshold {
        s.push(("threshold_pct".into(), Some(toml::Value::Float(v))));
    }
    if let Some(v) = &cli.forced_reset {
        if v == "null" {
            s.push(("forced_resets_at".into(), None));
        } else {
            let epoch: i64 = v
                .parse()
                .with_context(|| format!("invalid epoch for --forced-reset: '{v}'"))?;
            s.push(("forced_resets_at".into(), Some(toml::Value::Integer(epoch))));
        }
    }
    if let Some(v) = &cli.herdr {
        s.push(("herdr_bin".into(), Some(toml::Value::String(v.clone()))));
    }
    if let Some(v) = &cli.session {
        if v == "null" {
            s.push(("herdr_session".into(), None));
        } else {
            s.push(("herdr_session".into(), Some(toml::Value::String(v.clone()))));
        }
    }
    if let Some(v) = &cli.kind {
        s.push((
            "herdr_agent_kind".into(),
            Some(toml::Value::String(v.clone())),
        ));
    }
    if let Some(v) = &cli.resume_msg {
        s.push((
            "resume_message".into(),
            Some(toml::Value::String(v.clone())),
        ));
    }
    if let Some(v) = &cli.prompt {
        s.push((
            "delegation_prompt".into(),
            Some(toml::Value::String(v.clone())),
        ));
    }
    if cli.no_install_hook {
        s.push((
            "install_statusline_hook".into(),
            Some(toml::Value::Boolean(false)),
        ));
    }
    if let Some(p) = &cli.state_file {
        s.push((
            "state_path".into(),
            Some(toml::Value::String(p.display().to_string())),
        ));
    }
    if let Some(p) = &cli.statusline {
        s.push((
            "statusline_json_path".into(),
            Some(toml::Value::String(p.display().to_string())),
        ));
    }
    if let Some(v) = &cli.settings {
        if v == "null" {
            s.push(("claude_settings_path".into(), None));
        } else {
            s.push((
                "claude_settings_path".into(),
                Some(toml::Value::String(v.clone())),
            ));
        }
    }
    Ok(s)
}

fn toml_inline(v: &toml::Value) -> String {
    match v {
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::String(s) => format!("'{s}'"),
        other => other.to_string(),
    }
}

/// Escribe los settings en el archivo de config (creándolo si no existe),
/// con escritura atómica. "null" (None) elimina la clave. El daemon
/// recarga el archivo por mtime (hot-reload).
fn apply_config_settings(path: &Path, entries: &[(String, Option<toml::Value>)]) -> Result<()> {
    let mut table: toml::Value = if path.exists() {
        let raw =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("{} is not valid TOML", path.display()))?
    } else {
        toml::Value::Table(Default::default())
    };
    let root = table
        .as_table_mut()
        .context("config root is not a TOML table")?;

    for (key, value) in entries {
        match value {
            Some(v) => {
                root.insert(key.clone(), v.clone());
            }
            None => {
                root.remove(key);
            }
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, toml::to_string_pretty(&table)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;

    for (k, v) in entries {
        match v {
            Some(value) => println!("  {k} = {}", toml_inline(value)),
            None => println!("  {k} = {}", painted("(removed)", RED)),
        }
    }
    for (k, v) in entries {
        if k == "delegation" {
            match v {
                Some(toml::Value::Boolean(true)) => {
                    println!(
                        "{}",
                        painted(
                            "Delegation is now ON (the star; auto-resume keeps working).",
                            GREEN
                        )
                    );
                }
                Some(toml::Value::Boolean(false)) => {
                    println!(
                        "{}",
                        painted("Delegation is now OFF (plain auto-resume).", RED)
                    );
                }
                _ => {}
            }
        }
    }
    println!("Config actualizado en {}", path.display());
    Ok(())
}

fn load_state(path: &Path) -> Result<GuardState> {
    let p = resolve_path(path);
    if p.exists() {
        Ok(serde_json::from_str(&fs::read_to_string(p)?).unwrap_or_default())
    } else {
        Ok(GuardState::default())
    }
}

fn save_state(path: &Path, state: &GuardState) -> Result<()> {
    let p = resolve_path(path);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(p, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

/// Modo hook del statusLine de Claude Code: lee el payload JSON que Claude
/// Code injects on stdin on every render and persists it atomically
/// (write a tmp + rename, para que el daemon nunca lea un archivo cortado)
/// en statusline_json_path. Reemplaza al statusline_writer.sh externo:
/// en ~/.claude/settings.json -> "statusLine": {"type": "command",
/// "command": "climax --write-statusline"}.
/// Prints nothing to stdout: Claude Code might use that output as the
/// statusline content.
fn write_statusline(config: &Config) -> Result<()> {
    let mut raw = String::new();
    io::stdin()
        .read_to_string(&mut raw)
        .context("reading stdin from the hook")?;
    let payload = raw.trim();
    if payload.is_empty() {
        bail!("empty stdin: Claude Code sent no statusLine payload");
    }
    serde_json::from_str::<Value>(payload).context("statusLine payload is not valid JSON")?;

    let path = resolve_path(&config.statusline_json_path);
    let tmp = path.with_extension("tmp");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&tmp, payload).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

// ─────────────────────────────────────────────
// Auto-install del hook statusLine en el settings.json de Claude Code
// ─────────────────────────────────────────────

/// El comando exacto que el hook debe tener para considerarse "de climax".
const STATUSLINE_HOOK_COMMAND: &str = "climax --write-statusline";

/// Ruta del settings.json de usuario de Claude Code. Respeta
/// CLAUDE_CONFIG_DIR if set; otherwise ~/.claude/settings.json.
fn claude_settings_path(config: &Config) -> PathBuf {
    if let Some(p) = &config.claude_settings_path {
        return resolve_path(p);
    }
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(dir).join("settings.json");
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join(".claude/settings.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookState {
    /// The hook is ours and active.
    Present,
    /// The settings file does not exist.
    NoSettings,
    /// Existe pero sin statusLine.
    Missing,
    /// El statusLine apunta a otro comando: no se pisa.
    Other,
    /// The file exists but is not valid JSON.
    Invalid,
}

/// Read-only inspection of the hook state (fitness for --status).
fn hook_state(settings: &Path) -> HookState {
    if !settings.exists() {
        return HookState::NoSettings;
    }
    let raw = match fs::read_to_string(settings) {
        Ok(r) => r,
        Err(_) => return HookState::Invalid,
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return HookState::Invalid;
    };
    match v.get("statusLine") {
        None => HookState::Missing,
        Some(sl) => {
            let cmd = sl.get("command").and_then(|x| x.as_str()).unwrap_or("");
            let typ = sl.get("type").and_then(|x| x.as_str()).unwrap_or("");
            if typ == "command" && cmd == STATUSLINE_HOOK_COMMAND {
                HookState::Present
            } else {
                HookState::Other
            }
        }
    }
}

/// Instala el statusLine hook en settings.json preservando el resto del
/// contenido. Escribe con tmp+rename y saca un backup previo
/// (settings.json.climax.bak) la primera vez. Falla sin tocar nada si el
/// existing file is not valid JSON.
fn install_statusline_hook(settings: &Path) -> Result<bool> {
    let mut value = if settings.exists() {
        let raw = fs::read_to_string(settings)
            .with_context(|| format!("reading {}", settings.display()))?;
        serde_json::from_str::<Value>(&raw)
            .with_context(|| format!("{} is not valid JSON", settings.display()))?
    } else {
        Value::Object(Default::default())
    };

    let obj = value
        .as_object_mut()
        .context("settings.json no es un objeto JSON")?;
    match obj.get("statusLine") {
        None => {}
        Some(sl) => {
            let cmd = sl.get("command").and_then(|x| x.as_str()).unwrap_or("");
            if cmd == STATUSLINE_HOOK_COMMAND {
                return Ok(false);
            }
            bail!(
                "statusLine already points to another command ({}); leaving it alone",
                cmd
            );
        }
    }
    obj.insert(
        "statusLine".to_string(),
        json!({ "type": "command", "command": STATUSLINE_HOOK_COMMAND }),
    );

    if settings.exists() {
        let backup = PathBuf::from(format!("{}.climax.bak", settings.display()));
        if !backup.exists() {
            fs::copy(settings, &backup)
                .with_context(|| format!("backup en {}", backup.display()))?;
            info!("Backup de settings.json creado en {}", backup.display());
        }
    }
    if let Some(parent) = settings.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = settings.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&value)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, settings).with_context(|| format!("renaming to {}", settings.display()))?;
    Ok(true)
}

/// Punto de entrada del auto-install al arrancar el daemon.
/// With dry_run it only reports what it would do, touching nothing.
fn run_hook_install(config: &Config, dry_run: bool) -> Result<()> {
    if !config.install_statusline_hook {
        debug!("install_statusline_hook=false, skip auto-install");
        return Ok(());
    }
    let settings = claude_settings_path(config);
    match hook_state(&settings) {
        HookState::Present => info!("Hook statusLine ya configurado en {}", settings.display()),
        HookState::Other => warn!(
            "Claude Code statusLine points to another command in {}; \
             leaving it alone. If you intend to use climax as the quota \
             guard, configure it by hand.",
            settings.display()
        ),
        HookState::Invalid => warn!(
            "{} is not valid JSON: not touching the file (fix it or set \
             install_statusline_hook=false).",
            settings.display()
        ),
        HookState::NoSettings | HookState::Missing => {
            if dry_run {
                info!(
                    "[dry-run] would install the statusLine hook in {}",
                    settings.display()
                );
                return Ok(());
            }
            match install_statusline_hook(&settings) {
                Ok(true) => info!("StatusLine hook installed at {}", settings.display()),
                Ok(false) => info!("StatusLine hook was already there ({})", settings.display()),
                Err(e) => warn!("No se pudo instalar el hook: {:#}", e),
            }
        }
    }
    Ok(())
}

fn wait_duration(reset_at: i64, margin_secs: u64) -> Duration {
    let target = reset_at + margin_secs as i64;
    Duration::from_secs((target - Utc::now().timestamp()).max(5) as u64)
}

// ─────────────────────────────────────────────
// Servicio systemd de usuario (--install-service / --uninstall-service)
// ─────────────────────────────────────────────

const SERVICE_UNIT_NAME: &str = "climax.service";

fn user_systemd_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(dir).join("systemd/user")
    } else {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".config/systemd/user")
    }
}

/// Stable path where climax copies itself when installed as a
/// servicio: $XDG_BIN_HOME, $XDG_DATA_HOME/bin, o ~/.local/bin.
fn executable_install_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_BIN_HOME") {
        return PathBuf::from(dir).join("climax");
    }
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(dir).join("bin/climax");
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join(".local/bin/climax")
}

/// Copia el binario en ejecución a `path` (como archivo real, no symlink)
/// para que el servicio y el hook no dependan de dónde viva el código
/// fuente. No-op si ya está ahí. Devuelve true si copió.
fn self_install_to(path: &Path) -> Result<bool> {
    let src = std::env::current_exe().context("resolving the running binary")?;
    let src = fs::canonicalize(&src).unwrap_or(src);

    if let Ok(dst) = fs::canonicalize(path) {
        // Skip solo si el destino YA ES un archivo real idéntico (un
        // symlink no basta: hay que materializar la copia para que el
        // servicio no dependa de la ubicación del proyecto).
        if dst == src && !path.is_symlink() {
            return Ok(false);
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("reemplazando {}", path.display()))?;
    }
    fs::copy(&src, path).with_context(|| format!("copiando a {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod +x {}", path.display()))?;
    Ok(true)
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("systemctl")
        .args(args)
        .output()
        .context("running systemctl (is systemd installed?)")?;
    if !out.status.success() {
        bail!(
            "'systemctl {}' failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Instala climax como servicio de usuario de systemd con autorun en boot
/// (WantedBy=default.target: arranca al login; con `loginctl enable-linger`
/// también sin sesión abierta). Idempotente: en cada corrida regraba el unit
/// apuntando al binario actual, recarga systemd y reinicia el servicio.
/// Solo Linux/systemd: en macOS/otros se usa el binario directo (daemon).
fn install_service() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!(
            "--install-service only applies to Linux with systemd. On this \
             system run the binary directly (e.g.: nohup climax &)"
        );
    }
    let bin_path = executable_install_path();
    match self_install_to(&bin_path) {
        Ok(true) => println!("Binary installed {}", bin_path.display()),
        Ok(false) => println!("Binary already up to date: {}", bin_path.display()),
        Err(e) => println!(
            "Warning: could not copy the binary ({:#}); the unit will use the current binary.",
            e
        ),
    }

    let exe = if bin_path.exists() {
        bin_path.clone()
    } else {
        let exe = std::env::current_exe().context("resolviendo la ruta del binario")?;
        fs::canonicalize(&exe).unwrap_or(exe)
    };
    let exe_str = exe.display().to_string();
    let exe_quoted = if exe_str.contains(' ') {
        format!("\"{}\"", exe_str.replace('"', r#"\""#))
    } else {
        exe_str.clone()
    };

    let unit_dir = user_systemd_dir();
    fs::create_dir_all(&unit_dir).with_context(|| format!("creating {}", unit_dir.display()))?;
    let unit_path = unit_dir.join(SERVICE_UNIT_NAME);

    let unit = format!(
        "# Generated by 'climax --install-service' — do not edit by hand; it is \
         # regenerated on every install and the binary is copied to {}.\n\
         [Unit]\n\
         Description=Climax - Claude Code quota guard (JSON hook + herdr)\n\
         After=default.target\n\
         \n\
         [Service]\n\
         # User services start with a minimal PATH; without this they would \n         # not find herdr (or other ~/.local/bin tools).\n\
         Environment=PATH=%h/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n\
         ExecStart={exe_quoted}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        bin_path.display()
    );
    fs::write(&unit_path, unit).with_context(|| format!("writing {}", unit_path.display()))?;
    println!("Unit created: {}", unit_path.display());

    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", SERVICE_UNIT_NAME])?;
    println!("Enabled for boot (systemctl --user enable)");

    // Autorun sin login (boot): linger es best-effort, no crítico.
    if let Ok(env_user) = std::env::var("USER") {
        match std::process::Command::new("loginctl")
            .args(["enable-linger", &env_user])
            .output()
        {
            Ok(o) if o.status.success() => {
                println!("Login-less autorun enabled (loginctl enable-linger)");
            }
            Ok(o) => println!(
                "Warning: 'loginctl enable-linger' could not complete: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => println!(
                "Warning: loginctl not available ({e}); the service will start at login anyway"
            ),
        }
    }

    run_systemctl(&["--user", "restart", SERVICE_UNIT_NAME])?;
    println!(
        "Service restarted with the new version: {}",
        SERVICE_UNIT_NAME
    );

    println!();
    println!("Estado     : systemctl --user status climax.service");
    println!("Logs       : journalctl --user -u climax.service -f");
    println!("Uninstall : climax --uninstall-service");
    Ok(())
}

/// Detiene, deshabilita y elimina el unit del servicio. Idempotente:
/// if it was not installed, reports and exits without error.
fn uninstall_service() -> Result<()> {
    let unit_path = user_systemd_dir().join(SERVICE_UNIT_NAME);

    for step in [
        vec!["--user", "stop", SERVICE_UNIT_NAME],
        vec!["--user", "disable", SERVICE_UNIT_NAME],
    ] {
        if let Err(e) = run_systemctl(&step) {
            println!("(non-critical) {:#}", e);
        }
    }
    let _ = run_systemctl(&["--user", "daemon-reload"]);

    if unit_path.exists() {
        fs::remove_file(&unit_path).with_context(|| format!("removing {}", unit_path.display()))?;
        println!("Unit removed: {}", unit_path.display());
    } else {
        println!("No unit was installed (nothing to remove).");
    }

    if let Ok(env_user) = std::env::var("USER") {
        let _ = std::process::Command::new("loginctl")
            .args(["disable-linger", &env_user])
            .output();
    }

    println!("climax.service uninstalled.");
    println!(
        "Note: the binary in {} is kept (the statusLine hook keeps working).",
        executable_install_path().display()
    );
    Ok(())
}

fn print_status(info: &RateInfo, config: &Config) {
    println!(
        "used_pct       : {}",
        painted(
            &format!("{:.1}%", info.used_pct),
            if info.hard_limit_hit || info.used_pct >= 99.9 {
                RED
            } else if info.used_pct >= config.threshold_pct {
                YELLOW
            } else {
                GREEN
            }
        )
    );
    println!(
        "hard_limit     : {}",
        painted(
            if info.hard_limit_hit {
                "true (blocked)"
            } else {
                "false"
            },
            if info.hard_limit_hit { RED } else { GREEN }
        )
    );
    if let Some(ts) = info.resets_at {
        let dt = DateTime::from_timestamp(ts, 0)
            .map(|d| d.with_timezone(&Local).to_rfc3339())
            .unwrap_or_default();
        let remaining = ts - Utc::now().timestamp();
        println!("resets_at      : {} ({})", ts, dt);
        println!(
            "remaining_secs : {}",
            if remaining <= config.warning_lead_time_secs as i64 {
                painted(&format!("{remaining} (warning in ~{remaining}s)"), YELLOW)
            } else {
                remaining.to_string()
            }
        );
    } else {
        println!("resets_at      : (desconocido)");
    }
}
