use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};
use tracing::{error, info, warn};

use crate::error::{Result, SafeAgentError};
use crate::tunnel::TunnelUrl;

use super::rhai_runtime;

/// Minimum interval between full reconciliation scans. Prevents excessive
/// filesystem I/O when reconcile() is called every tick and after every message.
const RECONCILE_COOLDOWN: Duration = Duration::from_secs(30);

/// Manifest describing a skill, read from `skill.toml` in the skill directory.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SkillManifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// "daemon" (long-running) or "oneshot" (run once and exit).
    #[serde(default = "default_skill_type")]
    pub skill_type: String,
    /// Whether the skill should be started automatically.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Entry point relative to the skill directory (default: "main.py").
    #[serde(default = "default_entrypoint")]
    pub entrypoint: String,
    /// Python virtual-environment policy.
    ///
    /// - `"auto"` (default) — create a `.venv/` when `requirements.txt` exists.
    /// - `"always"` — always create a `.venv/`, even without `requirements.txt`.
    /// - `"never"` — install into the system Python (legacy behaviour).
    #[serde(default = "default_venv")]
    pub venv: String,
    /// Extra environment variables to pass to the skill process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Credentials this skill requires. Each entry declares a credential
    /// by name with a human-readable description and whether it's required.
    #[serde(default)]
    pub credentials: Vec<CredentialSpec>,
}

/// Declares a credential that a skill needs.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CredentialSpec {
    /// Environment variable name the credential is passed as.
    pub name: String,
    /// Human-readable label shown in the dashboard.
    #[serde(default)]
    pub label: String,
    /// Description / help text.
    #[serde(default)]
    pub description: String,
    /// Whether the skill cannot function without this credential.
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_skill_type() -> String {
    "daemon".to_string()
}
fn default_true() -> bool {
    true
}
fn default_entrypoint() -> String {
    "main.py".to_string()
}
fn default_venv() -> String {
    "auto".to_string()
}

/// Handle to a running skill — either an external child process or an
/// in-process Rhai script on a blocking thread.
enum SkillHandle {
    /// External process (Python, Node.js, shell).
    Process(Child),
    /// Embedded Rhai script running on a `spawn_blocking` thread.
    Embedded {
        task: tokio::task::JoinHandle<()>,
        cancel: Arc<AtomicBool>,
    },
}

/// Tracks a running skill.
struct RunningSkill {
    manifest: SkillManifest,
    handle: SkillHandle,
}

/// Manages skill lifecycle: discovery, start, stop, restart, credentials.
pub struct SkillManager {
    skills_dir: PathBuf,
    /// Additional skill directories contributed by plugins.  Scanned
    /// alongside `skills_dir` during `reconcile()`.
    extra_skill_dirs: Vec<PathBuf>,
    running: HashMap<String, RunningSkill>,
    telegram_bot_token: Option<String>,
    telegram_chat_id: Option<i64>,
    /// Stored credentials: skill_name -> { env_var_name -> value }
    credentials: HashMap<String, HashMap<String, String>>,
    credentials_path: PathBuf,
    /// Ngrok tunnel public URL receiver.
    tunnel_url: Option<TunnelUrl>,
    /// Skills that were manually stopped via API and should not be
    /// auto-restarted by `reconcile()` until explicitly started again.
    manually_stopped: std::collections::HashSet<String>,
    /// Last time a full reconciliation ran. Used for TTL cooldown.
    last_reconcile: Option<Instant>,
}

impl SkillManager {
    pub fn new(
        skills_dir: PathBuf,
        telegram_bot_token: Option<String>,
        telegram_chat_id: Option<i64>,
    ) -> Self {
        if let Err(e) = std::fs::create_dir_all(&skills_dir) {
            warn!(path = %skills_dir.display(), err = %e, "failed to create skills directory");
        }

        let credentials_path = skills_dir.join("credentials.json");
        let credentials = Self::load_credentials(&credentials_path);

        info!(
            path = %skills_dir.display(),
            stored_credentials = credentials.len(),
            "skill manager initialized"
        );

        Self {
            skills_dir,
            extra_skill_dirs: Vec::new(),
            running: HashMap::new(),
            telegram_bot_token,
            telegram_chat_id,
            credentials,
            credentials_path,
            tunnel_url: None,
            manually_stopped: std::collections::HashSet::new(),
            last_reconcile: None,
        }
    }

    /// Set the ngrok tunnel URL receiver so running (and future) skills
    /// receive `TUNNEL_URL` / `PUBLIC_URL` in their environment.
    pub fn set_tunnel_url(&mut self, url: TunnelUrl) {
        self.tunnel_url = Some(url);
    }

    /// Register an additional directory to scan for subprocess skills.
    ///
    /// Called during startup after the plugin registry discovers subprocess
    /// skill directories from loaded plugins.  Duplicates are ignored.
    pub fn add_skill_dir(&mut self, dir: PathBuf) {
        if !self.extra_skill_dirs.contains(&dir) {
            info!(path = %dir.display(), "registered plugin subprocess skill dir");
            self.extra_skill_dirs.push(dir);
        }
    }

    fn load_credentials(path: &Path) -> HashMap<String, HashMap<String, String>> {
        match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => HashMap::new(),
        }
    }

    fn save_credentials(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.credentials)
            .map_err(|e| SafeAgentError::Config(format!("serialize credentials: {e}")))?;
        std::fs::write(&self.credentials_path, json)
            .map_err(|e| SafeAgentError::Io(e))?;
        Ok(())
    }

    /// Get stored credentials for a skill.
    pub fn get_credentials(&self, skill_name: &str) -> HashMap<String, String> {
        self.credentials.get(skill_name).cloned().unwrap_or_default()
    }

    /// Set a credential value for a skill and persist to disk.
    pub fn set_credential(&mut self, skill_name: &str, key: &str, value: &str) -> Result<()> {
        self.credentials
            .entry(skill_name.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
        self.save_credentials()
    }

    /// Delete a credential value for a skill and persist to disk.
    pub fn delete_credential(&mut self, skill_name: &str, key: &str) -> Result<()> {
        if let Some(creds) = self.credentials.get_mut(skill_name) {
            creds.remove(key);
            if creds.is_empty() {
                self.credentials.remove(skill_name);
            }
        }
        self.save_credentials()
    }

    /// Scan the skills directory (and any plugin-contributed directories),
    /// start new enabled skills, restart crashed ones, and stop skills whose
    /// directories have been deleted.
    ///
    /// Called every tick from the agent loop.
    pub async fn reconcile(&mut self) -> Result<()> {
        // TTL cooldown: skip full scan if we ran recently
        if let Some(last) = self.last_reconcile {
            if last.elapsed() < RECONCILE_COOLDOWN {
                return Ok(());
            }
        }

        // Reap finished processes first
        self.reap_finished().await;

        // Collect the names of skills that still exist on disk so we can
        // detect deletions after the scan.
        let mut on_disk: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Scan the primary user-managed skills directory (blocking I/O in spawn_blocking)
        let skills_dir = self.skills_dir.clone();
        let discovered = tokio::task::spawn_blocking(move || {
            scan_dir_blocking(&skills_dir)
        })
        .await
        .map_err(|e| SafeAgentError::Config(format!("reconcile scan panic: {e}")))?
        .unwrap_or_default();

        for (path, manifest, _) in discovered {
            on_disk.insert(manifest.name.clone());
            if !manifest.enabled {
                if self.running.contains_key(&manifest.name) {
                    info!(skill = %manifest.name, "stopping disabled skill");
                    self.stop_skill(&manifest.name).await;
                }
                continue;
            }
            if !self.running.contains_key(&manifest.name)
                && !self.manually_stopped.contains(&manifest.name)
            {
                self.start_skill(manifest, path).await;
            }
        }

        // Scan plugin-contributed subprocess skill directories.
        // Each entry is a single skill directory (not a parent of many),
        // so we scan the directory itself rather than iterating children.
        for dir in self.extra_skill_dirs.clone() {
            let manifest_path = dir.join("skill.toml");
            if !manifest_path.exists() {
                continue;
            }
            let dir_for_spawn = dir.clone();
            let manifest = tokio::task::spawn_blocking(move || {
                read_manifest_blocking(&dir_for_spawn.join("skill.toml"))
            })
            .await
            .map_err(|e| SafeAgentError::Config(format!("reconcile manifest read panic: {e}")))??;
            on_disk.insert(manifest.name.clone());
            if !manifest.enabled {
                if self.running.contains_key(&manifest.name) {
                    info!(skill = %manifest.name, "stopping disabled plugin skill");
                    self.stop_skill(&manifest.name).await;
                }
                continue;
            }
            if !self.running.contains_key(&manifest.name)
                && !self.manually_stopped.contains(&manifest.name)
            {
                self.start_skill(manifest, dir).await;
            }
        }

        // Stop any running skills whose directories were deleted
        let orphaned: Vec<String> = self
            .running
            .keys()
            .filter(|name| !on_disk.contains(name.as_str()))
            .cloned()
            .collect();

        for name in orphaned {
            info!(skill = %name, "skill directory removed, stopping orphaned process");
            self.stop_skill(&name).await;
        }

        self.last_reconcile = Some(Instant::now());
        Ok(())
    }

    /// Start a skill — either as an external process (Python, Node.js, shell)
    /// or as an embedded Rhai script.
    async fn start_skill(&mut self, manifest: SkillManifest, dir: PathBuf) {
        let entrypoint = dir.join(&manifest.entrypoint);
        if !entrypoint.exists() {
            warn!(
                skill = %manifest.name,
                entrypoint = %entrypoint.display(),
                "skill entrypoint not found"
            );
            return;
        }

        // Create skill data directory
        let _ = std::fs::create_dir_all(dir.join("data"));

        // Collect environment variables that apply to every skill type.
        let env_vars = self.collect_skill_env(&manifest, &dir);

        // ── Rhai scripts: run in-process ──────────────────────────────────
        if manifest.entrypoint.ends_with(".rhai") {
            self.start_rhai_skill(manifest, dir, entrypoint, env_vars).await;
            return;
        }

        // ── External process skills (Python, Node.js, shell) ─────────────

        let is_python = matches!(manifest.entrypoint.rsplit('.').next(), Some("py"));

        // -- Python: virtual-environment + requirements ────────────────────
        let venv_python = if is_python {
            self.setup_python_venv(&manifest, &dir).await
        } else {
            None
        };

        // -- Node.js: package.json install -────────────────────────────────
        let package_json = dir.join("package.json");
        if package_json.exists() {
            info!(skill = %manifest.name, "installing Node.js dependencies");
            let npm_cmd = if which_exists("pnpm") { "pnpm" } else { "npm" };
            let install = Command::new(npm_cmd)
                .arg("install")
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .status()
                .await;
            match install {
                Ok(s) if s.success() => info!(skill = %manifest.name, npm_cmd, "node dependencies installed"),
                Ok(s) => warn!(skill = %manifest.name, status = %s, npm_cmd, "npm install failed"),
                Err(e) => warn!(skill = %manifest.name, err = %e, npm_cmd, "npm install error"),
            }
        }

        // -- Determine interpreter -────────────────────────────────────────
        let interpreter: String = if let Some(ref vpy) = venv_python {
            vpy.clone()
        } else {
            match manifest.entrypoint.rsplit('.').next() {
                Some("py") => "python3".into(),
                Some("js" | "mjs" | "cjs") => "node".into(),
                _ => "sh".into(),
            }
        };

        let log_path = dir.join("skill.log");
        let log_file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(f) => f,
            Err(e) => {
                error!(skill = %manifest.name, err = %e, "failed to open skill log");
                return;
            }
        };
        let stderr_log = match log_file.try_clone() {
            Ok(f) => f,
            Err(e) => {
                error!(skill = %manifest.name, err = %e, "failed to clone log file handle");
                return;
            }
        };

        let mut cmd = Command::new(&interpreter);
        cmd.arg(&entrypoint)
            .current_dir(&dir)
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(stderr_log));

        // If running inside a venv, prepend the venv bin dir to PATH so
        // sub-processes spawned by the skill also resolve to venv packages.
        if let Some(ref vpy) = venv_python {
            let venv_bin = std::path::Path::new(vpy)
                .parent()
                .expect("venv python has parent dir");
            let sys_path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{}", venv_bin.display(), sys_path));
            cmd.env("VIRTUAL_ENV", dir.join(".venv"));
        }

        // Set all collected environment variables.
        for (k, v) in &env_vars {
            cmd.env(k, v);
        }

        // On Unix: set process group + apply resource limits (rlimit)
        #[cfg(unix)]
        {
            #[allow(unused_imports)]
            use std::os::unix::process::CommandExt;
            let limits = crate::security::ProcessLimits::skill();
            unsafe {
                cmd.pre_exec(move || {
                    libc::setpgid(0, 0);
                    crate::security::apply_process_limits(&limits)?;
                    Ok(())
                });
            }
        }

        match cmd.spawn() {
            Ok(child) => {
                info!(
                    skill = %manifest.name,
                    pid = ?child.id(),
                    %interpreter,
                    entrypoint = %manifest.entrypoint,
                    "skill started (with resource limits)"
                );
                self.running.insert(
                    manifest.name.clone(),
                    RunningSkill {
                        manifest,
                        handle: SkillHandle::Process(child),
                    },
                );
            }
            Err(e) => {
                error!(skill = %manifest.name, err = %e, "failed to start skill");
            }
        }
    }

    /// Set up a Python virtual environment for a skill if required.
    ///
    /// Returns `Some(path_to_venv_python)` if a venv was created/reused,
    /// or `None` if the skill should use the system Python.
    async fn setup_python_venv(
        &self,
        manifest: &SkillManifest,
        dir: &Path,
    ) -> Option<String> {
        let requirements = dir.join("requirements.txt");
        let venv_dir = dir.join(".venv");

        let want_venv = match manifest.venv.as_str() {
            "always" => true,
            "never" => false,
            // "auto" (default) — venv when requirements.txt exists
            _ => requirements.exists(),
        };

        if !want_venv {
            // Legacy path: install globally if there are requirements.
            if requirements.exists() {
                info!(skill = %manifest.name, "installing Python requirements (system-wide)");
                let install = Command::new("pip3")
                    .args(["install", "--no-cache-dir", "--break-system-packages", "-r"])
                    .arg(&requirements)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .status()
                    .await;
                match install {
                    Ok(s) if s.success() => {
                        info!(skill = %manifest.name, "requirements installed (system-wide)");
                    }
                    Ok(s) => warn!(skill = %manifest.name, status = %s, "pip install failed"),
                    Err(e) => warn!(skill = %manifest.name, err = %e, "pip install error"),
                }
            }
            return None;
        }

        // Determine which python3 binary to use for creating the venv.
        let python_bin = if which_exists("python3") {
            "python3"
        } else if which_exists("python") {
            "python"
        } else {
            warn!(skill = %manifest.name, "no python3 binary found; cannot create venv");
            return None;
        };

        // Create the venv if it doesn't already exist.
        let venv_python = if cfg!(windows) {
            venv_dir.join("Scripts").join("python.exe")
        } else {
            venv_dir.join("bin").join("python")
        };

        if !venv_python.exists() {
            info!(
                skill = %manifest.name,
                venv = %venv_dir.display(),
                "creating Python virtual environment"
            );
            let create = Command::new(python_bin)
                .args(["-m", "venv"])
                .arg(&venv_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .status()
                .await;
            match create {
                Ok(s) if s.success() => {
                    info!(skill = %manifest.name, "venv created");
                }
                Ok(s) => {
                    warn!(skill = %manifest.name, status = %s, "venv creation failed");
                    return None;
                }
                Err(e) => {
                    warn!(skill = %manifest.name, err = %e, "venv creation error");
                    return None;
                }
            }
        }

        // Upgrade pip inside the venv (best-effort, silent).
        let _ = Command::new(venv_python.as_os_str())
            .args(["-m", "pip", "install", "--upgrade", "pip"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        // Install requirements into the venv.
        if requirements.exists() {
            info!(
                skill = %manifest.name,
                venv = %venv_dir.display(),
                "installing Python requirements into venv"
            );
            let install = Command::new(venv_python.as_os_str())
                .args(["-m", "pip", "install", "--no-cache-dir", "-r"])
                .arg(&requirements)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .status()
                .await;
            match install {
                Ok(s) if s.success() => {
                    info!(skill = %manifest.name, "venv requirements installed");
                }
                Ok(s) => {
                    warn!(skill = %manifest.name, status = %s, "venv pip install failed");
                }
                Err(e) => {
                    warn!(skill = %manifest.name, err = %e, "venv pip install error");
                }
            }
        }

        Some(venv_python.to_string_lossy().into_owned())
    }

    /// Launch a Rhai skill on a blocking thread.
    async fn start_rhai_skill(
        &mut self,
        manifest: SkillManifest,
        dir: PathBuf,
        script_path: PathBuf,
        env_vars: HashMap<String, String>,
    ) {
        let log_path = dir.join("skill.log");
        let log_file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(f) => f,
            Err(e) => {
                error!(skill = %manifest.name, err = %e, "failed to open skill log for Rhai");
                return;
            }
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(rhai_runtime::RhaiSkillCtx {
            cancel: cancel.clone(),
            env_vars,
            data_dir: dir.join("data"),
            log_file: Arc::new(std::sync::Mutex::new(log_file)),
            telegram_token: self.telegram_bot_token.clone(),
            telegram_chat_id: self.telegram_chat_id.map(|id| id.to_string()),
        });

        let skill_name = manifest.name.clone();
        let task = tokio::task::spawn_blocking(move || {
            let engine = rhai_runtime::build_engine(ctx.clone());
            if let Err(e) = rhai_runtime::run_script(&engine, &script_path) {
                if !ctx.cancel.load(Ordering::Relaxed) {
                    eprintln!("[rhai-skill:{skill_name}] {e}");
                }
            }
        });

        info!(
            skill = %manifest.name,
            entrypoint = %manifest.entrypoint,
            "embedded Rhai skill started"
        );

        self.running.insert(
            manifest.name.clone(),
            RunningSkill {
                manifest,
                handle: SkillHandle::Embedded { task, cancel },
            },
        );
    }

    /// Collect all environment variables for a skill (system + manifest +
    /// credentials + tunnel).
    fn collect_skill_env(
        &self,
        manifest: &SkillManifest,
        dir: &Path,
    ) -> HashMap<String, String> {
        let mut env = HashMap::new();

        // Core skill env
        env.insert("SKILL_NAME".into(), manifest.name.clone());
        env.insert("SKILL_DIR".into(), dir.to_string_lossy().to_string());
        env.insert("SKILL_DATA_DIR".into(), dir.join("data").to_string_lossy().to_string());
        env.insert("SKILLS_DIR".into(), self.skills_dir.to_string_lossy().to_string());
        env.insert("PYTHONUNBUFFERED".into(), "1".into());

        // Telegram
        if let Some(ref token) = self.telegram_bot_token {
            env.insert("TELEGRAM_BOT_TOKEN".into(), token.clone());
        }
        if let Some(chat_id) = self.telegram_chat_id {
            env.insert("TELEGRAM_CHAT_ID".into(), chat_id.to_string());
        }

        // Manifest env
        for (k, v) in &manifest.env {
            env.insert(k.clone(), v.clone());
        }

        // Stored credentials
        if let Some(creds) = self.credentials.get(&manifest.name) {
            for (k, v) in creds {
                env.insert(k.clone(), v.clone());
            }
        }

        // Tunnel URL
        if let Some(ref tunnel) = self.tunnel_url {
            if let Some(ref url) = *tunnel.borrow() {
                env.insert("TUNNEL_URL".into(), url.clone());
                env.insert("PUBLIC_URL".into(), url.clone());
            }
        }

        env
    }

    /// Stop a running skill by name, killing the entire process group.
    pub async fn stop_skill(&mut self, name: &str) {
        if let Some(skill) = self.running.remove(name) {
            match skill.handle {
                SkillHandle::Process(mut child) => {
                    if let Some(pid) = child.id() {
                        info!(skill = %name, pid, "stopping skill (killing process group)");
                        #[cfg(unix)]
                        {
                            unsafe {
                                libc::kill(-(pid as i32), libc::SIGTERM);
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            unsafe {
                                libc::kill(-(pid as i32), libc::SIGKILL);
                            }
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = child.kill().await;
                        }
                    } else {
                        info!(skill = %name, "stopping skill");
                        let _ = child.kill().await;
                    }
                    let _ = child.wait().await;
                }
                SkillHandle::Embedded { task, cancel } => {
                    info!(skill = %name, "stopping embedded Rhai skill");
                    cancel.store(true, Ordering::Relaxed);
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }

    /// Stop a running skill and mark it as manually stopped so that
    /// `reconcile()` will not auto-restart it.  Call `start_skill_by_name`
    /// or `restart_skill_by_name` to clear the manual-stop flag.
    pub async fn stop_skill_manual(&mut self, name: &str) {
        self.manually_stopped.insert(name.to_string());
        self.stop_skill(name).await;
        info!(skill = %name, "skill manually stopped (will not auto-restart)");
    }

    /// Start a skill by name.  Clears any manual-stop flag, locates the
    /// skill directory and manifest, and launches the process.  Returns
    /// `Ok(true)` if the skill was started, `Ok(false)` if it was already
    /// running, or an error if the skill was not found or is disabled.
    pub async fn start_skill_by_name(&mut self, name: &str) -> Result<bool> {
        // Clear manual-stop flag regardless
        self.manually_stopped.remove(name);

        // Already running?
        if self.running.contains_key(name) {
            return Ok(false);
        }

        let dir = self.find_skill_dir(name).ok_or_else(|| {
            SafeAgentError::Config(format!("skill '{name}' not found"))
        })?;

        let manifest = self.read_manifest(&dir.join("skill.toml"))?;

        if !manifest.enabled {
            return Err(SafeAgentError::Config(format!(
                "skill '{name}' is disabled — enable it first"
            )));
        }

        self.start_skill(manifest, dir).await;
        Ok(true)
    }

    /// Restart a skill by name: stop it, then start it again.
    /// Clears any manual-stop flag.
    pub async fn restart_skill_by_name(&mut self, name: &str) -> Result<()> {
        self.manually_stopped.remove(name);
        self.stop_skill(name).await;
        // Brief pause for process cleanup
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let dir = self.find_skill_dir(name).ok_or_else(|| {
            SafeAgentError::Config(format!("skill '{name}' not found"))
        })?;
        let manifest = self.read_manifest(&dir.join("skill.toml"))?;
        self.start_skill(manifest, dir).await;
        Ok(())
    }

    /// Check running skills for any that have exited, and remove them so
    /// they can be restarted on the next reconcile.
    async fn reap_finished(&mut self) {
        let mut finished = Vec::new();

        for (name, skill) in &mut self.running {
            let done = match &mut skill.handle {
                SkillHandle::Process(child) => match child.try_wait() {
                    Ok(Some(status)) => {
                        if status.success() && skill.manifest.skill_type == "oneshot" {
                            info!(skill = %name, "oneshot skill completed");
                        } else if status.success() {
                            info!(skill = %name, "daemon skill exited (will restart)");
                        } else {
                            warn!(
                                skill = %name,
                                status = %status,
                                "skill exited with error (will restart)"
                            );
                        }
                        true
                    }
                    Ok(None) => false,
                    Err(e) => {
                        warn!(skill = %name, err = %e, "error checking skill status");
                        true
                    }
                },
                SkillHandle::Embedded { task, .. } => {
                    if task.is_finished() {
                        if skill.manifest.skill_type == "oneshot" {
                            info!(skill = %name, "oneshot Rhai skill completed");
                        } else {
                            info!(skill = %name, "Rhai skill exited (will restart)");
                        }
                        true
                    } else {
                        false
                    }
                }
            };

            if done {
                finished.push(name.clone());
            }
        }

        for name in &finished {
            self.running.remove(name);
        }
    }

    fn read_manifest(&self, path: &Path) -> Result<SkillManifest> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| SafeAgentError::Config(format!("read skill manifest: {e}")))?;
        toml::from_str(&contents)
            .map_err(|e| SafeAgentError::Config(format!("parse skill manifest: {e}")))
    }

    /// List all skills (running and discovered).
    pub fn list(&self) -> Vec<SkillStatus> {
        let mut result = Vec::new();

        let entries = match std::fs::read_dir(&self.skills_dir) {
            Ok(e) => e,
            Err(_) => return result,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join("skill.toml");
            if !manifest_path.exists() {
                continue;
            }

            if let Ok(manifest) = self.read_manifest(&manifest_path) {
                let name = manifest.name.clone();
                let running = self.running.contains_key(&name);
                let pid = self
                    .running
                    .get(&name)
                    .and_then(|s| match &s.handle {
                        SkillHandle::Process(child) => child.id(),
                        SkillHandle::Embedded { .. } => None,
                    });

                let stored = self.get_credentials(&name);
                let credential_status: Vec<CredentialStatus> = manifest
                    .credentials
                    .iter()
                    .map(|spec| {
                        let configured = stored.contains_key(&spec.name);
                        CredentialStatus {
                            name: spec.name.clone(),
                            label: if spec.label.is_empty() {
                                spec.name.clone()
                            } else {
                                spec.label.clone()
                            },
                            description: spec.description.clone(),
                            required: spec.required,
                            configured,
                        }
                    })
                    .collect();

                let stopped = self.manually_stopped.contains(&name);
                let has_venv = path.join(".venv").join("bin").join("python").exists();
                result.push(SkillStatus {
                    name,
                    description: manifest.description,
                    skill_type: manifest.skill_type,
                    enabled: manifest.enabled,
                    running,
                    pid,
                    manually_stopped: stopped,
                    has_venv,
                    credentials: credential_status,
                });
            }
        }

        result
    }

    /// Data needed for async list. Clone this, drop the lock, then call list_async.
    pub fn list_data(&self) -> (
        PathBuf,
        HashMap<String, Option<u32>>,
        HashMap<String, HashMap<String, String>>,
        std::collections::HashSet<String>,
    ) {
        let running_info: HashMap<String, Option<u32>> = self
            .running
            .iter()
            .map(|(name, skill)| {
                let pid = match &skill.handle {
                    SkillHandle::Process(c) => c.id(),
                    _ => None,
                };
                (name.clone(), pid)
            })
            .collect();
        (
            self.skills_dir.clone(),
            running_info,
            self.credentials.clone(),
            self.manually_stopped.clone(),
        )
    }

    /// List all skills using spawn_blocking for filesystem I/O. Call after
    /// cloning data via list_data() so the lock is not held across await.
    pub async fn list_async(
        skills_dir: PathBuf,
        running_info: HashMap<String, Option<u32>>,
        credentials: HashMap<String, HashMap<String, String>>,
        manually_stopped: std::collections::HashSet<String>,
    ) -> Vec<SkillStatus> {
        let discovered = tokio::task::spawn_blocking(move || scan_dir_blocking(&skills_dir))
            .await
            .ok()
            .flatten()
            .unwrap_or_default();

        let mut result = Vec::new();
        for (_path, manifest, has_venv) in discovered {
            let name = manifest.name.clone();
            let running = running_info.contains_key(&name);
            let pid = running_info.get(&name).and_then(|p| *p);
            let stored = credentials.get(&name).cloned().unwrap_or_default();
            let credential_status: Vec<CredentialStatus> = manifest
                .credentials
                .iter()
                .map(|spec| {
                    let configured = stored.contains_key(&spec.name);
                    CredentialStatus {
                        name: spec.name.clone(),
                        label: if spec.label.is_empty() {
                            spec.name.clone()
                        } else {
                            spec.label.clone()
                        },
                        description: spec.description.clone(),
                        required: spec.required,
                        configured,
                    }
                })
                .collect();
            let stopped = manually_stopped.contains(&name);
            result.push(SkillStatus {
                name,
                description: manifest.description,
                skill_type: manifest.skill_type,
                enabled: manifest.enabled,
                running,
                pid,
                manually_stopped: stopped,
                has_venv,
                credentials: credential_status,
            });
        }
        result
    }

    /// Get the directory path for a skill by name, scanning the skills directory.
    fn find_skill_dir(&self, name: &str) -> Option<PathBuf> {
        let entries = std::fs::read_dir(&self.skills_dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("skill.toml");
            if manifest_path.exists() {
                if let Ok(manifest) = self.read_manifest(&manifest_path) {
                    if manifest.name == name {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    /// Get detailed information about a skill.
    pub fn detail(&self, name: &str) -> Result<SkillDetail> {
        let dir = self.find_skill_dir(name).ok_or_else(|| {
            SafeAgentError::Config(format!("skill '{name}' not found"))
        })?;

        let manifest_path = dir.join("skill.toml");
        let manifest_raw = std::fs::read_to_string(&manifest_path)
            .map_err(|e| SafeAgentError::Io(e))?;
        let manifest = self.read_manifest(&manifest_path)?;

        let log_path = dir.join("skill.log");
        let log_tail = Self::tail_file(&log_path, 100);

        let running = self.running.contains_key(name);
        let pid = self.running.get(name).and_then(|s| match &s.handle {
            SkillHandle::Process(child) => child.id(),
            SkillHandle::Embedded { .. } => None,
        });

        let stored = self.get_credentials(name);
        let credential_status: Vec<CredentialStatus> = manifest
            .credentials
            .iter()
            .map(|spec| {
                let configured = stored.contains_key(&spec.name);
                CredentialStatus {
                    name: spec.name.clone(),
                    label: if spec.label.is_empty() {
                        spec.name.clone()
                    } else {
                        spec.label.clone()
                    },
                    description: spec.description.clone(),
                    required: spec.required,
                    configured,
                }
            })
            .collect();

        let stopped = self.manually_stopped.contains(name);
        let venv_dir = dir.join(".venv");
        let has_venv = venv_dir.join("bin").join("python").exists();
        let venv_path = if has_venv {
            Some(venv_dir.to_string_lossy().into_owned())
        } else {
            None
        };
        Ok(SkillDetail {
            status: SkillStatus {
                name: manifest.name.clone(),
                description: manifest.description.clone(),
                skill_type: manifest.skill_type.clone(),
                enabled: manifest.enabled,
                running,
                pid,
                manually_stopped: stopped,
                has_venv,
                credentials: credential_status,
            },
            manifest_raw,
            env: manifest.env.clone(),
            log_tail,
            dir: dir.to_string_lossy().to_string(),
            entrypoint: manifest.entrypoint.clone(),
            venv_path,
        })
    }

    /// Read the last N lines of a file, returning an empty string if the file doesn't exist.
    fn tail_file(path: &Path, max_lines: usize) -> String {
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        let lines: Vec<&str> = contents.lines().collect();
        let start = lines.len().saturating_sub(max_lines);
        lines[start..].join("\n")
    }

    /// Read skill log (last N lines).
    pub fn read_log(&self, name: &str, max_lines: usize) -> Result<String> {
        let dir = self.find_skill_dir(name).ok_or_else(|| {
            SafeAgentError::Config(format!("skill '{name}' not found"))
        })?;
        Ok(Self::tail_file(&dir.join("skill.log"), max_lines))
    }

    /// Update a skill's manifest with new TOML contents. Validates before writing.
    pub fn update_manifest(&self, name: &str, new_toml: &str) -> Result<()> {
        // Parse the new TOML to validate it
        let _parsed: SkillManifest = toml::from_str(new_toml)
            .map_err(|e| SafeAgentError::Config(format!("invalid skill manifest TOML: {e}")))?;

        let dir = self.find_skill_dir(name).ok_or_else(|| {
            SafeAgentError::Config(format!("skill '{name}' not found"))
        })?;

        std::fs::write(dir.join("skill.toml"), new_toml)
            .map_err(|e| SafeAgentError::Io(e))?;

        info!(skill = %name, "manifest updated from dashboard");
        Ok(())
    }

    /// Toggle a skill's enabled state. Returns the new enabled value.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<bool> {
        let dir = self.find_skill_dir(name).ok_or_else(|| {
            SafeAgentError::Config(format!("skill '{name}' not found"))
        })?;

        let manifest_path = dir.join("skill.toml");
        let contents = std::fs::read_to_string(&manifest_path)
            .map_err(|e| SafeAgentError::Io(e))?;

        // Parse, update, serialize
        let mut doc: toml::Value = toml::from_str(&contents)
            .map_err(|e| SafeAgentError::Config(format!("parse manifest: {e}")))?;

        if let Some(table) = doc.as_table_mut() {
            table.insert("enabled".to_string(), toml::Value::Boolean(enabled));
        }

        let new_contents = toml::to_string_pretty(&doc)
            .map_err(|e| SafeAgentError::Config(format!("serialize manifest: {e}")))?;

        std::fs::write(&manifest_path, new_contents)
            .map_err(|e| SafeAgentError::Io(e))?;

        info!(skill = %name, enabled, "skill enabled state changed");
        Ok(enabled)
    }

    /// Update a single environment variable in the skill manifest.
    pub fn set_env_var(&self, name: &str, key: &str, value: &str) -> Result<()> {
        let dir = self.find_skill_dir(name).ok_or_else(|| {
            SafeAgentError::Config(format!("skill '{name}' not found"))
        })?;

        let manifest_path = dir.join("skill.toml");
        let contents = std::fs::read_to_string(&manifest_path)
            .map_err(|e| SafeAgentError::Io(e))?;

        let mut doc: toml::Value = toml::from_str(&contents)
            .map_err(|e| SafeAgentError::Config(format!("parse manifest: {e}")))?;

        if let Some(table) = doc.as_table_mut() {
            let env = table
                .entry("env")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
            if let Some(env_table) = env.as_table_mut() {
                env_table.insert(key.to_string(), toml::Value::String(value.to_string()));
            }
        }

        let new_contents = toml::to_string_pretty(&doc)
            .map_err(|e| SafeAgentError::Config(format!("serialize manifest: {e}")))?;

        std::fs::write(&manifest_path, new_contents)
            .map_err(|e| SafeAgentError::Io(e))?;

        info!(skill = %name, key, "env var updated from dashboard");
        Ok(())
    }

    /// Delete an environment variable from the skill manifest.
    pub fn delete_env_var(&self, name: &str, key: &str) -> Result<()> {
        let dir = self.find_skill_dir(name).ok_or_else(|| {
            SafeAgentError::Config(format!("skill '{name}' not found"))
        })?;

        let manifest_path = dir.join("skill.toml");
        let contents = std::fs::read_to_string(&manifest_path)
            .map_err(|e| SafeAgentError::Io(e))?;

        let mut doc: toml::Value = toml::from_str(&contents)
            .map_err(|e| SafeAgentError::Config(format!("parse manifest: {e}")))?;

        if let Some(table) = doc.as_table_mut() {
            if let Some(env) = table.get_mut("env").and_then(|v| v.as_table_mut()) {
                env.remove(key);
            }
        }

        let new_contents = toml::to_string_pretty(&doc)
            .map_err(|e| SafeAgentError::Config(format!("serialize manifest: {e}")))?;

        std::fs::write(&manifest_path, new_contents)
            .map_err(|e| SafeAgentError::Io(e))?;

        info!(skill = %name, key, "env var deleted from dashboard");
        Ok(())
    }

    // -- Import / Delete ---------------------------------------------------

    /// Import a skill from a source into the skills directory.
    ///
    /// Supported sources:
    /// - **git**: clone a git repository URL
    /// - **path**: copy a local directory
    /// - **url**: download a `.tar.gz` or `.zip` archive from a URL
    ///
    /// An optional `name` override renames the skill directory (and updates
    /// the manifest). If omitted, the directory name is inferred from the
    /// source (repo basename, archive name, or directory name).
    ///
    /// Returns the skill name and directory path on success.
    pub async fn import_skill(
        &self,
        source: &str,
        location: &str,
        name_override: Option<&str>,
    ) -> Result<(String, PathBuf)> {
        let dest_name = match name_override {
            Some(n) if !n.is_empty() => sanitize_skill_name(n),
            _ => infer_name_from_source(source, location),
        };

        if dest_name.is_empty() {
            return Err(SafeAgentError::Config(
                "could not determine skill name from source".into(),
            ));
        }

        let dest = self.skills_dir.join(&dest_name);
        if dest.exists() {
            return Err(SafeAgentError::Config(format!(
                "skill directory '{}' already exists — delete or rename it first",
                dest_name,
            )));
        }

        match source {
            "git" => self.import_from_git(location, &dest).await?,
            "path" => self.import_from_path(location, &dest)?,
            "url" => self.import_from_url(location, &dest).await?,
            other => {
                return Err(SafeAgentError::Config(format!(
                    "unknown import source type: '{other}' (expected git, path, or url)"
                )));
            }
        }

        // Validate that a skill.toml exists after import
        let manifest_path = dest.join("skill.toml");
        if !manifest_path.exists() {
            // Check if the archive extracted into a single subdirectory
            // (common pattern: repo-name/skill.toml)
            if let Some(inner) = Self::find_nested_skill_dir(&dest) {
                // Move contents up one level
                Self::hoist_inner_dir(&inner, &dest)?;
            }
        }

        if !dest.join("skill.toml").exists() {
            // Clean up the directory we created
            let _ = std::fs::remove_dir_all(&dest);
            return Err(SafeAgentError::Config(
                "imported source does not contain a skill.toml manifest".into(),
            ));
        }

        // If a name override was given, patch the manifest
        if name_override.is_some() {
            let manifest_path = dest.join("skill.toml");
            if let Ok(contents) = std::fs::read_to_string(&manifest_path) {
                if let Ok(mut doc) = contents.parse::<toml::Value>() {
                    if let Some(table) = doc.as_table_mut() {
                        table.insert(
                            "name".to_string(),
                            toml::Value::String(dest_name.clone()),
                        );
                    }
                    if let Ok(new_toml) = toml::to_string_pretty(&doc) {
                        let _ = std::fs::write(&manifest_path, new_toml);
                    }
                }
            }
        }

        // Read the final manifest name
        let manifest = self.read_manifest(&dest.join("skill.toml"))?;
        info!(
            skill = %manifest.name,
            source,
            location,
            "skill imported successfully"
        );

        Ok((manifest.name, dest))
    }

    /// Clone a git repo into the destination directory.
    async fn import_from_git(&self, url: &str, dest: &Path) -> Result<()> {
        info!(url, "cloning skill from git");
        let output = Command::new("git")
            .args(["clone", "--depth", "1", url])
            .arg(dest)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| SafeAgentError::Config(format!("failed to run git: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SafeAgentError::Config(format!(
                "git clone failed: {stderr}"
            )));
        }

        // Remove the .git directory — we don't need the history
        let _ = std::fs::remove_dir_all(dest.join(".git"));
        Ok(())
    }

    /// Copy a local directory into the skills directory.
    fn import_from_path(&self, src: &str, dest: &Path) -> Result<()> {
        let src_path = std::path::Path::new(src);
        if !src_path.is_dir() {
            return Err(SafeAgentError::Config(format!(
                "source path '{src}' is not a directory"
            )));
        }
        copy_dir_recursive(src_path, dest)?;
        Ok(())
    }

    /// Download an archive from a URL and extract it.
    async fn import_from_url(&self, url: &str, dest: &Path) -> Result<()> {
        info!(url, "downloading skill archive");
        let response = reqwest::get(url)
            .await
            .map_err(|e| SafeAgentError::Config(format!("download failed: {e}")))?;

        if !response.status().is_success() {
            return Err(SafeAgentError::Config(format!(
                "download returned HTTP {}",
                response.status()
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| SafeAgentError::Config(format!("failed to read response: {e}")))?;

        std::fs::create_dir_all(dest).map_err(SafeAgentError::Io)?;

        if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
            Self::extract_tar_gz(&bytes, dest)?;
        } else if url.ends_with(".zip") {
            Self::extract_zip(&bytes, dest)?;
        } else {
            // Try tar.gz first, fall back to zip
            if Self::extract_tar_gz(&bytes, dest).is_err() {
                if Self::extract_zip(&bytes, dest).is_err() {
                    let _ = std::fs::remove_dir_all(dest);
                    return Err(SafeAgentError::Config(
                        "could not extract archive — unsupported format (expected .tar.gz or .zip)".into(),
                    ));
                }
            }
        }

        Ok(())
    }

    fn extract_tar_gz(data: &[u8], dest: &Path) -> Result<()> {
        let gz = flate2::read::GzDecoder::new(data);
        let mut archive = tar::Archive::new(gz);
        archive
            .unpack(dest)
            .map_err(|e| SafeAgentError::Config(format!("tar extract failed: {e}")))?;
        Ok(())
    }

    fn extract_zip(data: &[u8], dest: &Path) -> Result<()> {
        let cursor = std::io::Cursor::new(data);
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|e| SafeAgentError::Config(format!("zip open failed: {e}")))?;
        archive
            .extract(dest)
            .map_err(|e| SafeAgentError::Config(format!("zip extract failed: {e}")))?;
        Ok(())
    }

    /// Look for a single subdirectory that contains skill.toml (common when
    /// cloning repos or extracting archives that wrap everything in one dir).
    fn find_nested_skill_dir(dir: &Path) -> Option<PathBuf> {
        let entries: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();

        if entries.len() == 1 {
            let inner = entries[0].path();
            if inner.join("skill.toml").exists() {
                return Some(inner);
            }
        }
        None
    }

    /// Move all contents from `inner` up into `dest`, then remove the inner
    /// directory.
    fn hoist_inner_dir(inner: &Path, dest: &Path) -> Result<()> {
        for entry in std::fs::read_dir(inner).map_err(SafeAgentError::Io)?.flatten() {
            let from = entry.path();
            let name = entry.file_name();
            let to = dest.join(&name);
            std::fs::rename(&from, &to).map_err(SafeAgentError::Io)?;
        }
        let _ = std::fs::remove_dir(inner);
        Ok(())
    }

    /// Delete a skill directory entirely (stopping it first if running).
    pub async fn delete_skill(&mut self, name: &str) -> Result<()> {
        // Stop if running
        if self.running.contains_key(name) {
            self.stop_skill(name).await;
        }

        let dir = self.find_skill_dir(name).ok_or_else(|| {
            SafeAgentError::Config(format!("skill '{name}' not found"))
        })?;

        std::fs::remove_dir_all(&dir).map_err(SafeAgentError::Io)?;

        // Clean up credentials
        self.credentials.remove(name);
        self.save_credentials()?;

        info!(skill = %name, "skill deleted");
        Ok(())
    }

    /// Stop all running skills (called on shutdown).
    pub async fn shutdown(&mut self) {
        let names: Vec<String> = self.running.keys().cloned().collect();
        for name in names {
            self.stop_skill(&name).await;
        }
        info!("all skills stopped");
    }
}

#[derive(Debug, serde::Serialize)]
pub struct SkillStatus {
    pub name: String,
    pub description: String,
    pub skill_type: String,
    pub enabled: bool,
    pub running: bool,
    pub pid: Option<u32>,
    /// True if the skill was manually stopped via API and won't auto-restart.
    pub manually_stopped: bool,
    /// Whether a Python venv exists for this skill.
    pub has_venv: bool,
    pub credentials: Vec<CredentialStatus>,
}

#[derive(Debug, serde::Serialize)]
pub struct CredentialStatus {
    pub name: String,
    pub label: String,
    pub description: String,
    pub required: bool,
    pub configured: bool,
}

/// Detailed view of a skill, including manifest contents and log tail.
#[derive(Debug, serde::Serialize)]
pub struct SkillDetail {
    #[serde(flatten)]
    pub status: SkillStatus,
    /// Raw contents of skill.toml
    pub manifest_raw: String,
    /// The env map from the manifest
    pub env: HashMap<String, String>,
    /// Last N lines of skill.log
    pub log_tail: String,
    /// Absolute path to the skill directory
    pub dir: String,
    /// Entrypoint file name
    pub entrypoint: String,
    /// Path to the Python venv directory, if one exists.
    pub venv_path: Option<String>,
}

// -- Free helpers --------------------------------------------------------

/// Blocking scan of a skills directory. Returns (path, manifest, has_venv) for each
/// discovered skill. Used from spawn_blocking to avoid blocking the async runtime.
fn scan_dir_blocking(dir: &Path) -> Option<Vec<(PathBuf, SkillManifest, bool)>> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut result = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("skill.toml");
        if !manifest_path.exists() {
            continue;
        }
        if let Ok(manifest) = read_manifest_blocking(&manifest_path) {
            let has_venv = path.join(".venv").join("bin").join("python").exists();
            result.push((path, manifest, has_venv));
        }
    }
    Some(result)
}

/// Blocking read and parse of a skill manifest. Used from spawn_blocking.
fn read_manifest_blocking(path: &Path) -> Result<SkillManifest> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| SafeAgentError::Config(format!("read skill manifest: {e}")))?;
    toml::from_str(&contents)
        .map_err(|e| SafeAgentError::Config(format!("parse skill manifest: {e}")))
}

/// Sanitise a user-provided skill name to a filesystem-safe directory name.
fn sanitize_skill_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_lowercase()
}

/// Infer a reasonable directory name from the import source.
fn infer_name_from_source(source: &str, location: &str) -> String {
    let raw = match source {
        "git" => {
            // https://github.com/user/repo.git -> repo
            let base = location.rsplit('/').next().unwrap_or(location);
            base.trim_end_matches(".git").to_string()
        }
        "path" => {
            std::path::Path::new(location)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        }
        "url" => {
            let base = location.rsplit('/').next().unwrap_or(location);
            base.trim_end_matches(".tar.gz")
                .trim_end_matches(".tgz")
                .trim_end_matches(".zip")
                .to_string()
        }
        _ => String::new(),
    };
    sanitize_skill_name(&raw)
}

/// Check whether a command exists on `$PATH`.
fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Recursively copy a directory tree.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(SafeAgentError::Io)?;
    for entry in std::fs::read_dir(src).map_err(SafeAgentError::Io)?.flatten() {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(SafeAgentError::Io)?;
        }
    }
    Ok(())
}
