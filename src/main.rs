use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, Utc};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

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
    /// Segundos extra después del resets_at antes de mandar el resume.
    /// Chico a propósito: se quiere despertar apenas se libera la cuota
    /// (el reset suele cumplirse a tiempo; 15s cubren skew de reloj).
    #[serde(default = "default_margin")]
    safety_margin_secs: u64,
    /// Cada cuántos segundos se chequea el JSON. Se clampea a un mínimo
    /// razonable en runtime para no saturar CPU ni el socket de herdr.
    #[serde(default = "default_poll")]
    poll_interval_secs: u64,

    /// Binario de herdr a invocar (por si no está en PATH con ese nombre exacto).
    #[serde(default = "default_herdr_bin")]
    herdr_bin: String,
    /// Sesión nombrada de herdr, si corresponde (se pasa como HERDR_SESSION).
    /// Dejar en None para usar la sesión default.
    herdr_session: Option<String>,
    /// Kind de agente que buscamos con `herdr agent list` cuando no hay
    /// target explícito (ver herdr_agent_target). Ver `herdr agent start --help`
    /// para la lista de kinds soportados; el nuestro es "claude".
    #[serde(default = "default_herdr_agent_kind")]
    herdr_agent_kind: String,
    /// Nombre de agente vivo o pane_id explícito (p.ej. "w1:p1" o el nombre
    /// que le pusiste con `herdr agent start <name> ...` / `agent rename`).
    /// Si está seteado, NO se hace autodetección — se usa tal cual.
    /// Necesario si tenés más de un agente Claude vivo a la vez.
    herdr_agent_target: Option<String>,

    /// Texto exacto del prompt de delegación. Si no está en el config,
    /// se usa el default embebido en el binario (DEFAULT_DELEGATION_PROMPT).
    #[serde(default = "default_delegation_prompt")]
    delegation_prompt: String,
    /// Si false, NO se inyecta el prompt de delegación antes del bloqueo
    /// (el auto-resume de "continue" sigue funcionando). Útil si no
    /// querés que el agente delegue a otros.
    #[serde(default = "default_true")]
    delegation_enabled: bool,
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
            poll_interval_secs: default_poll(),
            herdr_bin: default_herdr_bin(),
            herdr_session: None,
            herdr_agent_kind: default_herdr_agent_kind(),
            herdr_agent_target: None,
            delegation_prompt: default_delegation_prompt(),
            delegation_enabled: default_true(),
            resume_message: default_resume_msg(),
            state_path: default_state_path(),
            statusline_json_path: default_statusline_path(),
            install_statusline_hook: default_true(),
            claude_settings_path: None,
        }
    }
}

const DEFAULT_DELEGATION_PROMPT: &str = r#"URGENTE — CUOTA AGOTADA EN MINUTOS. Esta sesión de Claude Code se va a bloquear por la ventana de 5 horas y no vuelve hasta el reset. Tenés UN solo trabajo: maximizar el trabajo autónomo de los demás agentes sin vos. NADA de implementar, NADA de chatear, NADA de pedir permiso, NADA de tokens gastados describiendo lo que hacés.
PLAN OBLIGATORIO (hacelo YA, antes del bloqueo):
1. Prepará tu sesión: detené cualquier tarea larga en curso y garantizá que tu sesión quede en un estado fácil de retomar (revisá el working tree, no dejés cambios a medio hacer sin commit).
2. Generá un plan de trabajo de al menos 200 horas (o el máximo razonable del proyecto actual), en un solo archivo:
   - PWD/<PROYECTO>-delegation-plan.md
   - Tareas claras, priorizadas por valor y por independencia, cada una con criterio de "done" verificable, en el orden exacto en que debe ejecutarlas un agente nuevo.
   - Incluí al final un bloque "CONTEXTO" con: dónde está el código, convenciones del repo, cómo se corre build/tests, y el estado actual del trabajo.
3. Delegá TODO eso con herdr (tenés HERDR_ENV=1 y el skill):
   a) herdr pane split --current --direction right --cwd "$PWD" --no-focus
      → capturá el pane-id que devuelve la salida.
   b) herdr agent start opc-deleg --kind opencode --pane <pane-id> --timeout 300000
      → esperá que devuelva éxito (agente listo y detectado en el pane).
   c) herdr agent prompt opc-deleg "Trabajá de forma autónoma. Leé <plan> y ejecutalo. Criterios de done en el plan. Actualizá el state file (abajo) al terminar cada tarea. Si topás un bloqueo que requiere decisión de negocio, resolvelo con la mejor alternativa razonable y seguí, documentando la decisión." --wait
      → confirmá que el prompt fue tomado (no lo des por sentado).
   d) Si hay dos o más áreas totalmente independientes, repetí a,b,c por área (máximo 3 agentes). NUNCA partas una misma tarea entre dos agentes.
4. Si el skill de herdr no está disponible, delegá por el fallback que tengas configurado (OpenCode vía otra vía, subagente, MCP de coding). No dejes de delegar.
5. Actualizá HANDOFF.md (o el equivalente en el root del proyecto):
   - Cómo retomar tu propio trabajo cuando vuelvas (qué hiciste, dónde quedó, qué probaste).
   - Qué se delegó y a quién (nombre de agente + pane-id), con qué plan y el estado de cada área.
   - Qué falta hacer cuando vuelva la cuota.
6. Cuando todo esté delegado y confirmado: esperá en silencio el bloqueo. No sigas consumiendo tokens; no rehagás trabajo delegado; no escribas resúmenes largos. Una frase de una línea es suficiente.
Cuando vuelva el reset (lo va a manejar el guard automáticamente), la primera acción va a ser retomar desde HANDOFF.md, respetando que lo delegado lo está haciendo el otro agente."#;

// ─────────────────────────────────────────────
// State
// ─────────────────────────────────────────────
#[derive(Debug, Default, Serialize, Deserialize)]
struct GuardState {
    last_injected_reset_at: Option<i64>,
    last_hard_limit_reset_at: Option<i64>,
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
#[derive(Parser, Debug)]
#[command(
    name = "climax",
    version,
    about = "Claude Code quota guard (JSON hook + auto-resume, orquestado sobre herdr)"
)]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[arg(long)]
    status: bool,
    #[arg(long)]
    dry_run: bool,
    /// Modo hook del statusLine de Claude Code: lee el payload JSON que
    /// Claude Code envía por stdin y lo persiste atómicamente en
    /// statusline_json_path (default ~/.claude/statusline-cache.json).
    /// Reemplaza por completo al statusline_writer.sh externo.
    #[arg(long)]
    write_statusline: bool,
    /// Instala climax como servicio de usuario de systemd con autorun en
    /// boot (login + linger) y lo arranca. Idempotente.
    #[arg(long)]
    install_service: bool,
    /// Detiene, deshabilita y elimina el servicio de systemd.
    #[arg(long)]
    uninstall_service: bool,
    /// Edita la configuración (crea el archivo si no existe) y termina.
    /// Formato: --set clave=valor, repetible. El daemon recarga el archivo
    /// solo cuando cambia (hot-reload, sin reinicio).
    #[arg(long = "set", value_name = "KEY=VALUE")]
    set: Vec<String>,
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
            bail!("--install-service y --uninstall-service son mutuamente excluyentes");
        }
        if cli.status || cli.dry_run || cli.write_statusline {
            bail!("--install-service/--uninstall-service no se combinan con otros modos");
        }
        return if cli.install_service {
            install_service()
        } else {
            uninstall_service()
        };
    }

    let config_path: PathBuf = cli.config.clone().unwrap_or_else(default_config_path);

    if !cli.set.is_empty() {
        if cli.status || cli.dry_run || cli.write_statusline {
            bail!("--set no se combina con --status/--dry-run/--write-statusline");
        }
        return config_set(&config_path, &cli.set);
    }

    let mut config = load_config_from(&config_path)?;
    let dry_run = cli.dry_run;

    if cli.write_statusline {
        return write_statusline(&config);
    }

    // Piso duro para el intervalo de polling: nunca menos de 5s, sin
    // importar lo que diga el config.
    clamp_poll(&mut config);

    if cli.status {
        let info = gather_rate_info(&config)?;
        print_status(&info);
        println!(
            "delegation     : {}",
            if config.delegation_enabled {
                "activada"
            } else {
                "desactivada"
            }
        );
        println!(
            "statusline_hook: {}",
            match hook_state(&claude_settings_path(&config)) {
                HookState::Present => "instalado".to_string(),
                HookState::NoSettings => "no hay settings.json".to_string(),
                HookState::Missing => "falta (auto-install al arrancar el daemon)".to_string(),
                HookState::Other => "apunta a otro comando".to_string(),
                HookState::Invalid => "JSON inválido".to_string(),
            }
        );
        match resolve_target(&config).await {
            Ok(target) => {
                println!("herdr_target   : {target}");
                match get_agent_status(&config, &target).await {
                    Ok(status) => println!("herdr_status   : {status}"),
                    Err(e) => println!("herdr_status   : (error: {e:#})"),
                }
            }
            Err(e) => println!("herdr_target   : (no resuelto: {e:#})"),
        }
        return Ok(());
    }

    info!("climax iniciado (JSON hook + herdr, sin scraping de UI)");
    info!(
        "warning_lead = {}s | poll = {}s | margin = {}s | agent_kind = {}",
        config.warning_lead_time_secs,
        config.poll_interval_secs,
        config.safety_margin_secs,
        config.herdr_agent_kind
    );

    if let Err(e) = run_hook_install(&config, dry_run) {
        warn!("Chequeo/instalación del hook: {:#}", e);
    }

    if !resolve_path(&config.statusline_json_path).exists() {
        warn!(
            "No existe {} todavía: el hook lo va a generar con el primer render \
             de Claude Code.",
            config.statusline_json_path.display()
        );
    }

    let mut state = load_state(&config.state_path)?;
    let mut cached_target: Option<String> = None;
    let mut last_config_mtime = file_mtime(&config_path);

    loop {
        // Hot-reload: si el archivo de config cambió (por --set o a mano),
        // se recarga en el próximo ciclo sin reiniciar el servicio.
        if let Some(mtime) = file_mtime(&config_path) {
            if Some(mtime) != last_config_mtime {
                last_config_mtime = Some(mtime);
                match load_config_from(&config_path) {
                    Ok(new_config) => {
                        config = new_config;
                        clamp_poll(&mut config);
                        cached_target = None;
                        info!(
                            "Config recargada desde {} (poll={}s, delegation_enabled={})",
                            config_path.display(),
                            config.poll_interval_secs,
                            config.delegation_enabled
                        );
                    }
                    Err(e) => warn!("No se pudo recargar la config: {:#}", e),
                }
            }
        }

        match run_once(&config, &mut state, &mut cached_target, dry_run).await {
            Ok(Action::Continue) => {}
            Ok(Action::SleepUntil(reset_at)) => {
                let wait = wait_duration(reset_at, config.safety_margin_secs);
                info!(
                    "Hard limit detectado. Esperando hasta reset + margin (~{}s)...",
                    wait.as_secs()
                );
                sleep(wait).await;

                let resumed = if dry_run {
                    info!("[dry-run] mandaría resume: {}", config.resume_message);
                    true
                } else {
                    match ensure_target(&config, &mut cached_target).await {
                        Ok(target) => {
                            info!("Enviando resume a '{}': {}", target, config.resume_message);
                            match send_to_herdr(
                                &config,
                                &target,
                                &config.resume_message,
                                None,
                                None,
                            )
                            .await
                            {
                                Ok(_) => true,
                                Err(e) => {
                                    error!("Error enviando resume: {:#}", e);
                                    cached_target = None;
                                    false
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                "No se pudo resolver el agente de herdr para el resume: {:#}",
                                e
                            );
                            false
                        }
                    }
                };

                if resumed {
                    state.last_hard_limit_reset_at = Some(reset_at);
                    save_state(&config.state_path, &state)?;
                } else {
                    // No lo marcamos como manejado: el próximo ciclo va a
                    // recalcular wait_duration (que da un mínimo de 5s) y
                    // reintenta, en vez de quedarse colgado para siempre.
                    warn!("Resume no confirmado; se reintentará en el próximo ciclo.");
                }
            }
            Err(e) => warn!("Error en ciclo: {:#}", e),
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

    // 1. Hard limit: dormir y resumir
    if info.hard_limit_hit {
        if let Some(reset_at) = info.resets_at {
            if state.last_hard_limit_reset_at == Some(reset_at) {
                // Resume ya enviado para esta ventana. Si pasó de sobra el
                // reset y el JSON sigue reportando el bloqueo, el resets_at
                // del hook está desactualizado (o la ventana se corrió):
                // lo avisamos UNA vez por proceso y seguimos polleando.
                if now - reset_at > (config.safety_margin_secs as i64) + 60 {
                    warn!(
                        "Resume ya enviado para reset_at={} pero el límite sigue \
                         activo: el resets_at del hook está desactualizado o la \
                         ventana se corrió. El próximo ciclo de poll va a tomar \
                         el nuevo resets_at y reintentar.",
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
                debug!("Ya se inyectó para esta ventana, skip");
                return Ok(Action::Continue);
            }
        }

        if !config.delegation_enabled {
            info!(
                "Delegación desactivada (delegation_enabled=false): se omite el \
                 prompt de delegación (el auto-resume sigue activo)"
            );
            state.last_injected_reset_at = Some(reset_at);
            save_state(&config.state_path, state)?;
            return Ok(Action::Continue);
        }

        info!(
            "¡Aviso de bloqueo inminente! (remaining ~{}s) - inyectando delegación urgente",
            reset_at - now
        );

        if dry_run {
            info!("[dry-run] inyectaría el prompt de delegación");
        } else {
            let target = ensure_target(config, cached_target)
                .await
                .context("resolviendo agente de herdr para inyectar el aviso")?;
            if let Err(e) =
                send_to_herdr(config, &target, &config.delegation_prompt, None, None).await
            {
                *cached_target = None;
                return Err(e).context("falló la inyección del prompt via herdr");
            }
            info!("Prompt de delegación enviado a '{}'", target);
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
        debug!("JSON de statusline todavía no existe: {:?}", path);
        return Ok(RateInfo::default());
    }
    let data = fs::read_to_string(&path).with_context(|| format!("leyendo {}", path.display()))?;
    let v: Value = serde_json::from_str(&data).context("parseando JSON del statusline")?;

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

    // Claude a veces manda ms en vez de s
    let resets_at = resets_raw.map(|ts| {
        if ts > 1_000_000_000_000 {
            ts / 1000
        } else {
            ts
        }
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
// herdr: descubrimiento y envío (reemplaza todo el bloque tmux)
// ─────────────────────────────────────────────

#[derive(Debug, Clone)]
struct HerdrAgentEntry {
    target: String, // name si existe, si no pane_id
    kind: Option<String>,
}

fn herdr_command(config: &Config) -> Command {
    let mut cmd = Command::new(&config.herdr_bin);
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

/// Resuelve el target de `herdr agent ...` (nombre único o pane_id).
/// Si `herdr_agent_target` está seteado en el config, se usa directo.
/// Si no, se lista con `herdr agent list` y se filtra por `herdr_agent_kind`.
/// Ambigüedad (0 o >1 matches) es un error explícito, no una adivinanza.
async fn resolve_target(config: &Config) -> Result<String> {
    if let Some(explicit) = &config.herdr_agent_target {
        return Ok(explicit.clone());
    }

    let output = herdr_command(config)
        .args(["agent", "list"])
        .output()
        .await
        .context("ejecutando 'herdr agent list' (¿está herdr en el PATH y el server corriendo?)")?;

    if !output.status.success() {
        bail!(
            "'herdr agent list' falló: {}",
            herdr_error_message(&output.stderr)
        );
    }

    let v: Value =
        serde_json::from_slice(&output.stdout).context("parseando JSON de 'herdr agent list'")?;
    let agents = parse_agent_list(&v);

    let matches: Vec<&HerdrAgentEntry> = agents
        .iter()
        .filter(|a| a.kind.as_deref() == Some(config.herdr_agent_kind.as_str()))
        .collect();

    match matches.len() {
        0 => bail!(
            "No se encontró ningún agente kind='{}' vivo en herdr. Vistos: {:?}. \
             Seteá herdr_agent_target en el config de climax si el kind detectado no coincide.",
            config.herdr_agent_kind,
            agents
                .iter()
                .map(|a| format!("{}({})", a.target, a.kind.as_deref().unwrap_or("?")))
                .collect::<Vec<_>>()
        ),
        1 => Ok(matches[0].target.clone()),
        n => bail!(
            "Hay {} agentes kind='{}' vivos ({:?}). Seteá herdr_agent_target para desambiguar.",
            n,
            config.herdr_agent_kind,
            matches.iter().map(|a| a.target.clone()).collect::<Vec<_>>()
        ),
    }
}

async fn ensure_target(config: &Config, cached: &mut Option<String>) -> Result<String> {
    if let Some(t) = cached {
        return Ok(t.clone());
    }
    let t = resolve_target(config).await?;
    info!("Agente objetivo resuelto: {}", t);
    *cached = Some(t.clone());
    Ok(t)
}

/// Envía texto + Enter de forma atómica vía `herdr agent prompt`.
/// Sin shell, sin buffers, sin C-c previo: `agent prompt` funciona
/// aunque el agente esté trabajando.
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
            "'herdr agent prompt' a '{}' falló: {}",
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

/// Mtime (modificado) de un archivo, o None si no existe/falla.
fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Claves aceptadas por `--set`, para no crear claves fantasma por typos.
const SETTABLE_KEYS: &[&str] = &[
    "threshold_pct",
    "warning_lead_time_secs",
    "safety_margin_secs",
    "poll_interval_secs",
    "herdr_bin",
    "herdr_session",
    "herdr_agent_kind",
    "herdr_agent_target",
    "delegation_prompt",
    "delegation_enabled",
    "resume_message",
    "install_statusline_hook",
    "claude_settings_path",
    "state_path",
    "statusline_json_path",
];

/// Convierte el valor string de --set en un valor TOML tipado
/// (bool → Boolean, entero → Integer, flotante → Float, resto → String).
fn config_value(s: &str) -> toml::Value {
    match s {
        "true" => toml::Value::Boolean(true),
        "false" => toml::Value::Boolean(false),
        "null" => toml::Value::String("\u{0}NULL\u{0}".to_string()),
        _ => s
            .parse::<i64>()
            .map(toml::Value::Integer)
            .or_else(|_| s.parse::<f64>().map(toml::Value::Float))
            .unwrap_or_else(|_| toml::Value::String(s.to_string())),
    }
}

/// Modo `--set key=value`: mergea las claves en el archivo de config
/// (creándolo si no existe), con escritura atómica. "null" borra la clave
/// (para los campos opcionales). El daemon recarga el archivo por mtime.
fn config_set(path: &Path, entries: &[String]) -> Result<()> {
    let mut table: toml::Value = if path.exists() {
        let raw =
            fs::read_to_string(path).with_context(|| format!("leyendo {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("{} no es TOML válido", path.display()))?
    } else {
        toml::Value::Table(Default::default())
    };
    let root = table
        .as_table_mut()
        .context("la raíz del config no es una tabla TOML")?;

    for entry in entries {
        let (key, value) = entry
            .split_once('=')
            .with_context(|| format!("formato inválido en --set '{entry}' (esperado key=value)"))?;
        let key = key.trim();
        if !SETTABLE_KEYS.contains(&key) {
            bail!(
                "clave desconocida: '{key}'. Válidas: {}",
                SETTABLE_KEYS.join(", ")
            );
        }
        let value = value.trim();
        if value == "null" {
            root.remove(key);
        } else {
            root.insert(key.to_string(), config_value(value));
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creando {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, toml::to_string_pretty(&table)?)
        .with_context(|| format!("escribiendo {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("renombrando a {}", path.display()))?;

    for entry in entries {
        let (k, v) = entry.split_once('=').expect("ya validado");
        println!("  {k} = {}", v.trim());
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
/// Code inyecta por stdin en cada render y lo persiste atómicamente
/// (write a tmp + rename, para que el daemon nunca lea un archivo cortado)
/// en statusline_json_path. Reemplaza al statusline_writer.sh externo:
/// en ~/.claude/settings.json -> "statusLine": {"type": "command",
/// "command": "climax --write-statusline"}.
/// No imprime nada a stdout: Claude Code podría usar esa salida como
/// contenido del statusline.
fn write_statusline(config: &Config) -> Result<()> {
    let mut raw = String::new();
    io::stdin()
        .read_to_string(&mut raw)
        .context("leyendo stdin del hook")?;
    let payload = raw.trim();
    if payload.is_empty() {
        bail!("stdin vacío: Claude Code no envió el payload del statusLine");
    }
    serde_json::from_str::<Value>(payload).context("payload del statusLine no es JSON válido")?;

    let path = resolve_path(&config.statusline_json_path);
    let tmp = path.with_extension("tmp");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&tmp, payload).with_context(|| format!("escribiendo {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("renombrando a {}", path.display()))?;
    Ok(())
}

// ─────────────────────────────────────────────
// Auto-install del hook statusLine en el settings.json de Claude Code
// ─────────────────────────────────────────────

/// El comando exacto que el hook debe tener para considerarse "de climax".
const STATUSLINE_HOOK_COMMAND: &str = "climax --write-statusline";

/// Ruta del settings.json de usuario de Claude Code. Respeta
/// CLAUDE_CONFIG_DIR si está seteado; si no, ~/.claude/settings.json.
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
    /// El hook es nuestro y está activo.
    Present,
    /// No existe el archivo de settings.
    NoSettings,
    /// Existe pero sin statusLine.
    Missing,
    /// El statusLine apunta a otro comando: no se pisa.
    Other,
    /// El archivo existe pero no es JSON válido.
    Invalid,
}

/// Inspección de solo-lectura del estado del hook (fitness para --status).
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
/// archivo existente no es JSON válido.
fn install_statusline_hook(settings: &Path) -> Result<bool> {
    let mut value = if settings.exists() {
        let raw = fs::read_to_string(settings)
            .with_context(|| format!("leyendo {}", settings.display()))?;
        serde_json::from_str::<Value>(&raw)
            .with_context(|| format!("{} no es JSON válido", settings.display()))?
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
            bail!("statusLine ya apunta a otro comando ({}); no se pisa", cmd);
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
        .with_context(|| format!("escribiendo {}", tmp.display()))?;
    fs::rename(&tmp, settings).with_context(|| format!("renombrando a {}", settings.display()))?;
    Ok(true)
}

/// Punto de entrada del auto-install al arrancar el daemon.
/// Con dry_run solo reporta lo que haría, sin tocar nada.
fn run_hook_install(config: &Config, dry_run: bool) -> Result<()> {
    if !config.install_statusline_hook {
        debug!("install_statusline_hook=false, skip auto-install");
        return Ok(());
    }
    let settings = claude_settings_path(config);
    match hook_state(&settings) {
        HookState::Present => info!("Hook statusLine ya configurado en {}", settings.display()),
        HookState::Other => warn!(
            "statusLine de Claude Code apunta a otro comando en {}; no voy a pisarlo. \
             Si intentás usar climax como quota guard, configuralo a mano.",
            settings.display()
        ),
        HookState::Invalid => warn!(
            "{} no es JSON válido: no toco el archivo (arreglalo o seteá \
             install_statusline_hook=false).",
            settings.display()
        ),
        HookState::NoSettings | HookState::Missing => {
            if dry_run {
                info!(
                    "[dry-run] instalaría el statusLine hook en {}",
                    settings.display()
                );
                return Ok(());
            }
            match install_statusline_hook(&settings) {
                Ok(true) => info!("StatusLine hook instalado en {}", settings.display()),
                Ok(false) => info!("StatusLine hook ya estaba ({})", settings.display()),
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

/// Ruta estable donde climax se copia a sí mismo al instalarse como
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
    let src = std::env::current_exe().context("resolviendo el binario en ejecución")?;
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
        fs::create_dir_all(parent).with_context(|| format!("creando {}", parent.display()))?;
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
        .context("ejecutando systemctl (¿está instalado systemd?)")?;
    if !out.status.success() {
        bail!(
            "'systemctl {}' falló: {}",
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
fn install_service() -> Result<()> {
    let bin_path = executable_install_path();
    match self_install_to(&bin_path) {
        Ok(true) => println!("Binario instalado en {}", bin_path.display()),
        Ok(false) => println!("Binario ya actualizado en {}", bin_path.display()),
        Err(e) => println!(
            "Aviso: no se pudo copiar el binario ({:#}); el unit usará el path actual.",
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
    fs::create_dir_all(&unit_dir).with_context(|| format!("creando {}", unit_dir.display()))?;
    let unit_path = unit_dir.join(SERVICE_UNIT_NAME);

    let unit = format!(
        "# Generado por 'climax --install-service' no editar a mano: se regenera\n\
         # en cada instalación y el binario se copia a {}.\n\
         [Unit]\n\
         Description=Climax - Claude Code quota guard (JSON hook + herdr)\n\
         After=default.target\n\
         \n\
         [Service]\n\
         ExecStart={exe_quoted}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        bin_path.display()
    );
    fs::write(&unit_path, unit).with_context(|| format!("escribiendo {}", unit_path.display()))?;
    println!("Unit creado: {}", unit_path.display());

    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", SERVICE_UNIT_NAME])?;
    println!("Habilitado para arranque (systemctl --user enable)");

    // Autorun sin login (boot): linger es best-effort, no crítico.
    if let Ok(env_user) = std::env::var("USER") {
        match std::process::Command::new("loginctl")
            .args(["enable-linger", &env_user])
            .output()
        {
            Ok(o) if o.status.success() => {
                println!("Autorun sin login activado (loginctl enable-linger)");
            }
            Ok(o) => println!(
                "Aviso: 'loginctl enable-linger' no pudo completarse: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => println!(
                "Aviso: loginctl no disponible ({e}); el servicio arranca al hacer login igualmente"
            ),
        }
    }

    run_systemctl(&["--user", "restart", SERVICE_UNIT_NAME])?;
    println!(
        "Servicio reiniciado con la versión nueva: {}",
        SERVICE_UNIT_NAME
    );

    println!();
    println!("Estado     : systemctl --user status climax.service");
    println!("Logs       : journalctl --user -u climax.service -f");
    println!("Desinstalar: climax --uninstall-service");
    Ok(())
}

/// Detiene, deshabilita y elimina el unit del servicio. Idempotente:
/// si no estaba instalado, informa y termina sin error.
fn uninstall_service() -> Result<()> {
    let unit_path = user_systemd_dir().join(SERVICE_UNIT_NAME);

    for step in [
        vec!["--user", "stop", SERVICE_UNIT_NAME],
        vec!["--user", "disable", SERVICE_UNIT_NAME],
    ] {
        if let Err(e) = run_systemctl(&step) {
            println!("(no crítico) {:#}", e);
        }
    }
    let _ = run_systemctl(&["--user", "daemon-reload"]);

    if unit_path.exists() {
        fs::remove_file(&unit_path).with_context(|| format!("borrando {}", unit_path.display()))?;
        println!("Unit eliminado: {}", unit_path.display());
    } else {
        println!("No había unit instalado (nada que borrar).");
    }

    if let Ok(env_user) = std::env::var("USER") {
        let _ = std::process::Command::new("loginctl")
            .args(["disable-linger", &env_user])
            .output();
    }

    println!("climax.service desinstalado.");
    println!(
        "Nota: el binario en {} se conserva (el hook statusLine sigue funcionando).",
        executable_install_path().display()
    );
    Ok(())
}

fn print_status(info: &RateInfo) {
    println!("used_pct       : {:.1}%", info.used_pct);
    println!("hard_limit     : {}", info.hard_limit_hit);
    if let Some(ts) = info.resets_at {
        let dt = DateTime::from_timestamp(ts, 0)
            .map(|d| d.with_timezone(&Local).to_rfc3339())
            .unwrap_or_default();
        println!("resets_at      : {} ({})", ts, dt);
        println!("remaining_secs : {}", ts - Utc::now().timestamp());
    } else {
        println!("resets_at      : (desconocido)");
    }
}
