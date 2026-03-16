pub mod audit;
pub mod capabilities;
pub mod cost_tracker;
pub mod pii;
pub mod rate_limiter;
pub mod sandbox;
pub mod twofa;

#[cfg(target_os = "linux")]
pub mod seccomp;

#[cfg(target_os = "macos")]
pub mod seatbelt;

use std::path::{Path, PathBuf};

use reqwest::Url;
use tracing::{info, warn};

use crate::error::{Result, SafeAgentError};

// ===========================================================================
// SandboxedFs — path-jailed filesystem access
// ===========================================================================

/// Sandboxed filesystem — all file I/O is confined to the data directory.
#[derive(Debug, Clone)]
pub struct SandboxedFs {
    root: PathBuf,
}

impl SandboxedFs {
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        let root = root
            .canonicalize()
            .map_err(|e| SafeAgentError::SandboxViolation(format!("cannot canonicalize root: {e}")))?;
        Ok(Self { root })
    }

    /// Resolve a relative path within the sandbox. Rejects any path that escapes.
    pub fn resolve(&self, relative: &Path) -> Result<PathBuf> {
        if relative.is_absolute() {
            return Err(SafeAgentError::SandboxViolation(
                "absolute paths are not allowed".into(),
            ));
        }

        let candidate = self.root.join(relative);

        // Create parent dirs so canonicalize works on new files
        if let Some(parent) = candidate.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // For existing paths, canonicalize and check containment
        if candidate.exists() {
            let canonical = candidate.canonicalize()?;
            if !canonical.starts_with(&self.root) {
                return Err(SafeAgentError::SandboxViolation(format!(
                    "path escapes sandbox: {}",
                    relative.display()
                )));
            }
            return Ok(canonical);
        }

        // For new paths, canonicalize the parent and check
        if let Some(parent) = candidate.parent() {
            let canonical_parent = parent.canonicalize()?;
            if !canonical_parent.starts_with(&self.root) {
                return Err(SafeAgentError::SandboxViolation(format!(
                    "path escapes sandbox: {}",
                    relative.display()
                )));
            }
            let filename = candidate
                .file_name()
                .ok_or_else(|| SafeAgentError::SandboxViolation("invalid filename".into()))?;
            return Ok(canonical_parent.join(filename));
        }

        Err(SafeAgentError::SandboxViolation(
            "cannot resolve path".into(),
        ))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, relative: &Path, data: &[u8]) -> Result<()> {
        let path = self.resolve(relative)?;
        Ok(std::fs::write(path, data)?)
    }

    pub fn read_to_string(&self, relative: &Path) -> Result<String> {
        let path = self.resolve(relative)?;
        Ok(std::fs::read_to_string(path)?)
    }

}

// ===========================================================================
// PathJail — validates arbitrary paths are inside an allowed directory
// ===========================================================================

/// Validates that a given absolute or relative path resolves inside a jail root.
/// Used by Rhai extensions and anywhere that receives untrusted path strings.
#[derive(Debug, Clone)]
pub struct PathJail {
    root: PathBuf,
}

impl PathJail {
    /// Create a new PathJail. The root is canonicalized at construction time.
    pub fn new(root: PathBuf) -> Option<Self> {
        std::fs::create_dir_all(&root).ok()?;
        let root = root.canonicalize().ok()?;
        Some(Self { root })
    }

    /// Validate and resolve a path string. Returns `None` if the path escapes
    /// the jail or cannot be resolved.
    pub fn validate(&self, path: &str) -> Option<PathBuf> {
        let p = Path::new(path);

        // Reject obvious traversal patterns before any filesystem access
        let path_str = path.replace('\\', "/");
        if path_str.contains("/../")
            || path_str.starts_with("../")
            || path_str.ends_with("/..")
            || path_str == ".."
        {
            warn!(path = %path, jail = %self.root.display(), "path traversal rejected (pattern match)");
            return None;
        }

        let candidate = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        };

        // For existing paths, canonicalize and check
        if candidate.exists() {
            let canonical = candidate.canonicalize().ok()?;
            if canonical.starts_with(&self.root) {
                return Some(canonical);
            }
            warn!(
                path = %path,
                resolved = %canonical.display(),
                jail = %self.root.display(),
                "path escapes jail"
            );
            return None;
        }

        // For new paths, canonicalize the parent
        if let Some(parent) = candidate.parent() {
            std::fs::create_dir_all(parent).ok()?;
            let canonical_parent = parent.canonicalize().ok()?;
            if canonical_parent.starts_with(&self.root) {
                let filename = candidate.file_name()?;
                return Some(canonical_parent.join(filename));
            }
            warn!(
                path = %path,
                resolved_parent = %canonical_parent.display(),
                jail = %self.root.display(),
                "path parent escapes jail"
            );
        }

        None
    }

}

// ===========================================================================
// URL validation for Rhai extensions
// ===========================================================================

/// Validate a URL for Rhai HTTP functions. Blocks dangerous schemes and
/// private/internal network addresses.
pub fn validate_url(url: &str) -> std::result::Result<Url, String> {
    use std::net::Ipv4Addr;

    let parsed: Url = url.parse().map_err(|e| format!("invalid URL: {e}"))?;

    // Only allow http and https
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => return Err(format!("blocked URL scheme: {scheme}")),
    }

    // Block access to private/internal networks
    if let Some(host) = parsed.host_str() {
        let host_lower = host.to_lowercase();

        // Check domain-based blocklist
        if host_lower == "localhost"
            || host_lower.ends_with(".local")
            || host_lower.ends_with(".internal")
        {
            return Err(format!("blocked internal/private host: {host}"));
        }

        // Parse as IP address to catch IPv4, IPv6 loopback, and
        // IPv4-mapped IPv6 (e.g. ::ffff:127.0.0.1).
        // Strip brackets that some URL parsers leave on IPv6 literals.
        let bare_host = host_lower.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ipv4) = bare_host.parse::<Ipv4Addr>() {
            if is_private_ipv4(ipv4) {
                return Err(format!("blocked internal/private host: {host}"));
            }
        } else if let Ok(ipv6) = bare_host.parse::<std::net::Ipv6Addr>() {
            if ipv6.is_loopback() {
                return Err(format!("blocked internal/private host: {host}"));
            }
            if let Some(mapped) = ipv6.to_ipv4_mapped() {
                if is_private_ipv4(mapped) {
                    return Err(format!("blocked internal/private host: {host}"));
                }
            }
        }
    } else {
        return Err("URL has no host".into());
    }

    // Block access to the dashboard port
    if let Some(host) = parsed.host_str() {
        let host_lower = host.to_lowercase();
        if (host_lower == "localhost" || host_lower == "127.0.0.1")
            && parsed.port() == Some(3031)
        {
            return Err("blocked: cannot access dashboard from extensions".into());
        }
    }

    Ok(parsed)
}

fn is_private_ipv4(ip: std::net::Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
}


// ===========================================================================
// SQL guard — restrict dangerous SQL from Rhai extensions
// ===========================================================================

/// Validate SQL statements from Rhai extensions.
/// Blocks schema-destructive operations (DROP, ALTER, ATTACH, DETACH, VACUUM,
/// PRAGMA that writes, and LOAD_EXTENSION).
pub fn validate_sql(sql: &str) -> std::result::Result<(), String> {
    let upper = sql.trim().to_uppercase();

    // Remove leading comments
    let stripped = upper
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.starts_with("--"))
        .collect::<Vec<_>>()
        .join(" ");

    let dangerous_prefixes = [
        "DROP ",
        "ALTER ",
        "ATTACH ",
        "DETACH ",
        "VACUUM",
        "LOAD_EXTENSION",
        "CREATE INDEX",
        "DROP INDEX",
    ];

    for prefix in &dangerous_prefixes {
        if stripped.starts_with(prefix) {
            return Err(format!("blocked SQL operation: {prefix}"));
        }
    }

    // Block PRAGMA writes (allow PRAGMA reads like PRAGMA table_info)
    if stripped.starts_with("PRAGMA ") && stripped.contains('=') {
        return Err("blocked: PRAGMA writes not allowed from extensions".into());
    }

    // Block table-level destructive operations embedded in other statements
    let dangerous_anywhere = ["DROP TABLE", "ALTER TABLE", "ATTACH DATABASE"];
    for pattern in &dangerous_anywhere {
        if stripped.contains(pattern) {
            return Err(format!("blocked SQL pattern: {pattern}"));
        }
    }

    Ok(())
}

/// Validate SQL for read-only queries. Only SELECT, WITH, and EXPLAIN are allowed.
pub fn validate_sql_readonly(sql: &str) -> std::result::Result<(), String> {
    let upper = sql.trim().to_uppercase();
    let stripped = upper
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.starts_with("--"))
        .collect::<Vec<_>>()
        .join(" ");

    if stripped.starts_with("SELECT ")
        || stripped.starts_with("WITH ")
        || stripped.starts_with("EXPLAIN ")
    {
        Ok(())
    } else {
        Err(format!(
            "read-only DB: only SELECT/WITH/EXPLAIN allowed, got: {}",
            &sql[..sql.len().min(40)]
        ))
    }
}

// ===========================================================================
// Environment variable allowlist for Rhai
// ===========================================================================

/// Check whether an environment variable name is safe to read from Rhai.
pub fn is_safe_env_var(key: &str) -> bool {
    let allowed_prefixes = [
        "SKILL_",
        "SAFE_AGENT_",
        "TUNNEL_",
        "DASHBOARD_",
        "XDG_",
        "HOME",
        "LANG",
        "TZ",
        "PATH",
        "TERM",
        "NODE_",
        "PYTHON",
        "NVM_",
        "PYENV_",
    ];

    let blocked_exact = [
        "TELEGRAM_BOT_TOKEN",
        "JWT_SECRET",
        "DASHBOARD_PASSWORD",
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
        "DATABASE_URL",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "GITHUB_TOKEN",
        "GH_TOKEN",
    ];

    // Exact-match blocks take precedence
    if blocked_exact.iter().any(|b| key.eq_ignore_ascii_case(b)) {
        return false;
    }

    // Block anything containing SECRET, TOKEN, PASSWORD, KEY (case-insensitive)
    let upper = key.to_uppercase();
    if upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("_KEY")
        || upper.contains("CREDENTIAL")
        || upper.contains("AUTH")
    {
        return false;
    }

    // Allow prefixed vars
    allowed_prefixes.iter().any(|p| key.starts_with(p))
}

// ===========================================================================
// Process resource limits (Unix only)
// ===========================================================================

/// Resource limits to apply to child processes via pre_exec.
#[derive(Debug, Clone)]
pub struct ProcessLimits {
    /// Max virtual memory in bytes (RLIMIT_AS). Default: 2 GiB.
    pub max_memory_bytes: u64,
    /// Max file size in bytes (RLIMIT_FSIZE). Default: 256 MiB.
    pub max_file_size_bytes: u64,
    /// Max open file descriptors (RLIMIT_NOFILE). Default: 256.
    pub max_open_files: u64,
    /// Max CPU time in seconds (RLIMIT_CPU). Default: 300 (5 min).
    pub max_cpu_secs: u64,
    /// Max number of processes/threads (RLIMIT_NPROC). Default: 64.
    pub max_processes: u64,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 2 * 1024 * 1024 * 1024,   // 2 GiB
            max_file_size_bytes: 256 * 1024 * 1024,      // 256 MiB
            max_open_files: 256,
            max_cpu_secs: 300,
            max_processes: 64,
        }
    }
}

impl ProcessLimits {
    /// Restrictive limits for skill processes.
    pub fn skill() -> Self {
        Self {
            max_memory_bytes: 1024 * 1024 * 1024,        // 1 GiB
            max_file_size_bytes: 128 * 1024 * 1024,       // 128 MiB
            max_open_files: 128,
            max_cpu_secs: 600,
            max_processes: 32,
        }
    }
}

/// Apply resource limits as a pre_exec hook on Unix systems.
/// This function is async-signal-safe and suitable for use in `pre_exec`.
#[cfg(unix)]
pub unsafe fn apply_process_limits(limits: &ProcessLimits) -> std::io::Result<()> {
    use rlimit::Resource;

    let set = |resource: Resource, limit: u64| -> std::io::Result<()> {
        resource.set(limit, limit).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::Other, format!("setrlimit failed: {e}"))
        })
    };

    set(Resource::AS, limits.max_memory_bytes)?;
    set(Resource::FSIZE, limits.max_file_size_bytes)?;
    set(Resource::NOFILE, limits.max_open_files)?;
    set(Resource::CPU, limits.max_cpu_secs)?;
    set(Resource::NPROC, limits.max_processes)?;

    Ok(())
}

// ===========================================================================
// Capability dropping (Linux only)
// ===========================================================================

#[cfg(target_os = "linux")]
pub fn drop_capabilities() -> std::result::Result<(), String> {
    // CAP_LAST_CAP is 41 as of Linux 6.x; iterate 0..=41 to cover all.
    const CAP_LAST: i32 = 41;
    let mut dropped = 0;
    for cap in 0..=CAP_LAST {
        let ret = unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) };
        if ret == 0 {
            dropped += 1;
        }
        // EINVAL means the capability doesn't exist on this kernel — skip
    }
    info!(dropped, "bounding capabilities dropped");
    Ok(())
}

// ===========================================================================
// Landlock filesystem sandbox (Linux only)
// ===========================================================================

/// Apply Landlock filesystem restrictions to the current process.
/// This restricts filesystem access at the kernel level, providing defense-in-depth
/// even if application-level sandboxing has bugs.
///
/// The policy:
/// - Read-write: data_dir, config_dir, tmp
/// - Read-only: system paths (/usr, /lib, /etc, nvm, pyenv, skill dirs)
/// - Execute: system binaries, nvm/pyenv managed binaries
/// - Everything else: denied
#[cfg(target_os = "linux")]
pub fn apply_landlock(data_dir: &Path, config_dir: &Path) -> std::result::Result<(), String> {
    use landlock::{
        Access, AccessFs, BitFlags, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus, ABI,
    };

    let abi = ABI::V3;

    let read_only: BitFlags<AccessFs> = AccessFs::from_read(abi);
    let read_write: BitFlags<AccessFs> = AccessFs::from_all(abi);

    let status = Ruleset::default()
        .handle_access(read_write)
        .map_err(|e| format!("landlock ruleset: {e}"))?
        .create()
        .map_err(|e| format!("landlock create: {e}"))?
        // Read-write access to data directory
        .add_rule(PathBeneath::new(
            PathFd::new(data_dir).map_err(|e| format!("landlock pathfd data: {e}"))?,
            read_write,
        ))
        .map_err(|e| format!("landlock rule data_dir: {e}"))?
        // Read-write access to config directory
        .add_rule(PathBeneath::new(
            PathFd::new(config_dir).map_err(|e| format!("landlock pathfd config: {e}"))?,
            read_write,
        ))
        .map_err(|e| format!("landlock rule config_dir: {e}"))?
        // Read-write access to /tmp
        .add_rule(PathBeneath::new(
            PathFd::new("/tmp").map_err(|e| format!("landlock pathfd tmp: {e}"))?,
            read_write,
        ))
        .map_err(|e| format!("landlock rule /tmp: {e}"))?;

    // Read-only + execute for system paths containing binaries.
    let exec_paths = ["/usr", "/bin", "/sbin", "/lib", "/lib64"];
    let mut status = status;
    for p in &exec_paths {
        if Path::new(p).exists() {
            status = status
                .add_rule(PathBeneath::new(
                    PathFd::new(p).map_err(|e| format!("landlock pathfd {p}: {e}"))?,
                    read_only | AccessFs::Execute,
                ))
                .map_err(|e| format!("landlock rule {p}: {e}"))?;
        }
    }

    // Read-only system paths (no execute needed).
    let ro_paths = [
        "/etc", "/proc/self", "/dev/null", "/dev/zero", "/dev/urandom",
    ];
    for p in &ro_paths {
        if Path::new(p).exists() {
            status = status
                .add_rule(PathBeneath::new(
                    PathFd::new(p).map_err(|e| format!("landlock pathfd {p}: {e}"))?,
                    read_only,
                ))
                .map_err(|e| format!("landlock rule {p}: {e}"))?;
        }
    }

    // nvm / pyenv directories (read-only + execute)
    let nvm_dir = std::env::var("NVM_DIR").unwrap_or_default();
    let pyenv_root = std::env::var("PYENV_ROOT").unwrap_or_default();
    for dir in [&nvm_dir, &pyenv_root] {
        if !dir.is_empty() && Path::new(dir).exists() {
            status = status
                .add_rule(PathBeneath::new(
                    PathFd::new(dir.as_str()).map_err(|e| format!("landlock pathfd {dir}: {e}"))?,
                    read_only | AccessFs::Execute,
                ))
                .map_err(|e| format!("landlock rule {dir}: {e}"))?;
        }
    }

    // Home directory for config files (read-only outside data/config)
    if let Some(home) = dirs::home_dir() {
        if home.exists() {
            status = status
                .add_rule(PathBeneath::new(
                    PathFd::new(&home).map_err(|e| format!("landlock pathfd home: {e}"))?,
                    read_only,
                ))
                .map_err(|e| format!("landlock rule home: {e}"))?;
        }
    }

    let result = status
        .restrict_self()
        .map_err(|e| format!("landlock restrict_self: {e}"))?;

    match result.ruleset {
        RulesetStatus::FullyEnforced => {
            info!("landlock sandbox fully enforced");
            Ok(())
        }
        RulesetStatus::PartiallyEnforced => {
            warn!("landlock sandbox partially enforced (kernel may not support all features)");
            Ok(())
        }
        RulesetStatus::NotEnforced => {
            warn!("landlock not enforced (kernel too old or Landlock disabled)");
            Ok(())
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn apply_landlock(_data_dir: &Path, _config_dir: &Path) -> std::result::Result<(), String> {
    info!("landlock not available on this platform (Linux only)");
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_jail_blocks_traversal() {
        let tmp = std::env::temp_dir().join("test_jail");
        std::fs::create_dir_all(&tmp).unwrap();

        let jail = PathJail::new(tmp.clone()).unwrap();

        // Valid paths
        assert!(jail.validate("file.txt").is_some());
        assert!(jail.validate("subdir/file.txt").is_some());

        // Traversal attacks
        assert!(jail.validate("../etc/passwd").is_none());
        assert!(jail.validate("subdir/../../etc/passwd").is_none());
        assert!(jail.validate("..").is_none());

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_url_validation() {
        assert!(validate_url("https://example.com/api").is_ok());
        assert!(validate_url("http://example.com/api").is_ok());

        // Blocked schemes
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("ftp://example.com").is_err());

        // Blocked hosts
        assert!(validate_url("http://localhost:8080").is_err());
        assert!(validate_url("http://127.0.0.1:8080").is_err());
        assert!(validate_url("http://192.168.1.1").is_err());
        assert!(validate_url("http://10.0.0.1").is_err());
        assert!(validate_url("http://172.16.0.1").is_err());
    }

    #[test]
    fn test_sql_validation() {
        // Allowed
        assert!(validate_sql("SELECT * FROM memory").is_ok());
        assert!(validate_sql("INSERT INTO ext_data VALUES (1, 'test')").is_ok());
        assert!(validate_sql("UPDATE ext_data SET value = 'new' WHERE id = 1").is_ok());
        assert!(validate_sql("DELETE FROM ext_data WHERE id = 1").is_ok());
        assert!(validate_sql("CREATE TABLE ext_new (id INTEGER)").is_ok());

        // Blocked
        assert!(validate_sql("DROP TABLE memory").is_err());
        assert!(validate_sql("ALTER TABLE memory ADD COLUMN x TEXT").is_err());
        assert!(validate_sql("ATTACH DATABASE ':memory:' AS tmp").is_err());
        assert!(validate_sql("PRAGMA journal_mode=WAL").is_err());

        // Read-only validation
        assert!(validate_sql_readonly("SELECT * FROM memory").is_ok());
        assert!(validate_sql_readonly("INSERT INTO t VALUES (1)").is_err());
    }

    #[test]
    fn test_env_var_safety() {
        assert!(is_safe_env_var("SKILL_DIR"));
        assert!(is_safe_env_var("HOME"));
        assert!(is_safe_env_var("PATH"));
        assert!(is_safe_env_var("NVM_DIR"));

        assert!(!is_safe_env_var("JWT_SECRET"));
        assert!(!is_safe_env_var("TELEGRAM_BOT_TOKEN"));
        assert!(!is_safe_env_var("OPENROUTER_API_KEY"));
        assert!(!is_safe_env_var("ANTHROPIC_API_KEY"));
        assert!(!is_safe_env_var("MY_SECRET_VALUE"));
    }

    // -------------------------------------------------------------------------
    // SandboxedFs
    // -------------------------------------------------------------------------

    #[test]
    fn test_sandboxed_fs_new_and_root() {
        let tmp = std::env::temp_dir().join("test_sandboxed_fs");
        std::fs::create_dir_all(&tmp).unwrap();

        let sandbox = SandboxedFs::new(tmp.clone()).unwrap();
        let root = sandbox.root();
        assert!(root.ends_with("test_sandboxed_fs") || root.canonicalize().unwrap().ends_with("test_sandboxed_fs"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_sandboxed_fs_resolve_valid_paths() {
        let tmp = std::env::temp_dir().join("test_sandbox_resolve");
        std::fs::create_dir_all(&tmp).unwrap();

        let sandbox = SandboxedFs::new(tmp.clone()).unwrap();

        // Valid relative paths
        let p1 = sandbox.resolve(std::path::Path::new("file.txt")).unwrap();
        assert!(p1.ends_with("file.txt"));

        let subdir = tmp.join("subdir");
        std::fs::create_dir_all(&subdir).unwrap();
        let p2 = sandbox.resolve(std::path::Path::new("subdir/file.txt")).unwrap();
        let p2_str = p2.to_string_lossy();
        assert!(p2_str.contains("subdir") && p2_str.contains("file.txt"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_sandboxed_fs_resolve_rejects_traversal() {
        let tmp = std::env::temp_dir().join("test_sandbox_traversal");
        std::fs::create_dir_all(&tmp).unwrap();

        let sandbox = SandboxedFs::new(tmp.clone()).unwrap();

        assert!(sandbox.resolve(std::path::Path::new("../etc/passwd")).is_err());
        assert!(sandbox.resolve(std::path::Path::new("subdir/../../etc/passwd")).is_err());
        assert!(sandbox.resolve(std::path::Path::new("..")).is_err());
        assert!(sandbox.resolve(std::path::Path::new("a/../..")).is_err());

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_sandboxed_fs_resolve_rejects_absolute_paths() {
        let tmp = std::env::temp_dir().join("test_sandbox_absolute");
        std::fs::create_dir_all(&tmp).unwrap();

        let sandbox = SandboxedFs::new(tmp.clone()).unwrap();

        let abs = std::path::Path::new("/etc/passwd");
        assert!(abs.is_absolute());
        assert!(sandbox.resolve(abs).is_err());

        std::fs::remove_dir_all(&tmp).ok();
    }

    // -------------------------------------------------------------------------
    // ProcessLimits
    // -------------------------------------------------------------------------

    #[test]
    fn test_process_limits_default() {
        let limits = ProcessLimits::default();
        assert_eq!(limits.max_memory_bytes, 2 * 1024 * 1024 * 1024);   // 2 GiB
        assert_eq!(limits.max_file_size_bytes, 256 * 1024 * 1024);    // 256 MiB
        assert_eq!(limits.max_open_files, 256);
        assert_eq!(limits.max_cpu_secs, 300);
        assert_eq!(limits.max_processes, 64);
    }

    #[test]
    fn test_process_limits_skill() {
        let limits = ProcessLimits::skill();
        assert_eq!(limits.max_memory_bytes, 1024 * 1024 * 1024);       // 1 GiB
        assert_eq!(limits.max_file_size_bytes, 128 * 1024 * 1024);     // 128 MiB
        assert_eq!(limits.max_open_files, 128);
        assert_eq!(limits.max_cpu_secs, 600);
        assert_eq!(limits.max_processes, 32);
    }

    // -------------------------------------------------------------------------
    // validate_url edge cases
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_url_edge_cases() {
        // Empty string
        assert!(validate_url("").is_err());

        // Very long URL (potential overflow / abuse)
        let long = format!("https://example.com/{}", "a".repeat(10000));
        assert!(validate_url(&long).is_ok()); // Should still parse if valid

        // URLs with usernames (may bypass host checks in some parsers)
        assert!(validate_url("https://user:pass@example.com/path").is_ok());
        assert!(validate_url("https://user@localhost/path").is_err());

        // IPv6 addresses
        assert!(validate_url("https://[::1]/").is_err());
        assert!(validate_url("https://[2001:db8::1]/").is_ok());
        assert!(validate_url("http://[::ffff:127.0.0.1]/").is_err()); // IPv4-mapped loopback

        // More private ranges
        assert!(validate_url("http://169.254.1.1").is_err());
        assert!(validate_url("http://something.local").is_err());
        assert!(validate_url("http://host.internal").is_err());
    }

    // -------------------------------------------------------------------------
    // validate_sql edge cases
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_sql_edge_cases() {
        // Empty string
        assert!(validate_sql("").is_ok());
        assert!(validate_sql("   ").is_ok());

        // Case variations for DROP
        assert!(validate_sql("DROP TABLE x").is_err());
        assert!(validate_sql("drop TABLE x").is_err());
        assert!(validate_sql("DRoP TABLE x").is_err());
        assert!(validate_sql("DrOp table users").is_err());

        // Case variations for other dangerous ops
        assert!(validate_sql("ALTER TABLE x ADD c INT").is_err());
        assert!(validate_sql("alter table x add c int").is_err());
        assert!(validate_sql("VACUUM").is_err());
        assert!(validate_sql("vacuum").is_err());
    }

    // -------------------------------------------------------------------------
    // validate_sql_readonly
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_sql_readonly_cases() {
        assert!(validate_sql_readonly("SELECT * FROM t").is_ok());
        assert!(validate_sql_readonly("WITH cte AS (SELECT 1) SELECT * FROM cte").is_ok());
        assert!(validate_sql_readonly("EXPLAIN SELECT 1").is_ok());
        assert!(validate_sql_readonly("  SELECT 1  ").is_ok());
        assert!(validate_sql_readonly("-- comment\nSELECT 1").is_ok());

        assert!(validate_sql_readonly("INSERT INTO t VALUES (1)").is_err());
        assert!(validate_sql_readonly("UPDATE t SET x=1").is_err());
        assert!(validate_sql_readonly("DELETE FROM t").is_err());
        assert!(validate_sql_readonly("CREATE TABLE t (x INT)").is_err());
        assert!(validate_sql_readonly("").is_err());
    }

    // -------------------------------------------------------------------------
    // is_safe_env_var edge cases
    // -------------------------------------------------------------------------

    #[test]
    fn test_is_safe_env_var_edge_cases() {
        // Empty string
        assert!(!is_safe_env_var(""));

        // Mixed case blocked patterns
        assert!(!is_safe_env_var("jwt_secret"));
        assert!(!is_safe_env_var("JWT_SECRET"));
        assert!(!is_safe_env_var("Jwt_Secret"));
        assert!(!is_safe_env_var("MY_PASSWORD"));
        assert!(!is_safe_env_var("api_token"));
        assert!(!is_safe_env_var("DATABASE_CREDENTIAL"));

        // Pattern matches
        assert!(!is_safe_env_var("X_SECRET_Y"));
        assert!(!is_safe_env_var("SOME_AUTH_KEY"));
        assert!(!is_safe_env_var("PRIVATE_KEY"));

        // Allowed prefixes
        assert!(is_safe_env_var("SKILL_NAME"));
        assert!(is_safe_env_var("SAFE_AGENT_DATA"));
        assert!(is_safe_env_var("TUNNEL_URL"));
        assert!(is_safe_env_var("DASHBOARD_PORT"));
        assert!(is_safe_env_var("XDG_DATA_HOME"));
        assert!(is_safe_env_var("NODE_PATH"));
        assert!(is_safe_env_var("PYTHONPATH"));
    }

    // -------------------------------------------------------------------------
    // PathJail edge cases
    // -------------------------------------------------------------------------

    #[test]
    fn test_path_jail_new_and_validate_edge_cases() {
        let tmp = std::env::temp_dir().join("test_jail_edge");
        std::fs::create_dir_all(&tmp).unwrap();

        let jail = PathJail::new(tmp.clone()).unwrap();

        // Empty string - resolves to root (Path::new("") joined with root yields root)
        let res = jail.validate("");
        assert!(res.is_some());

        // Deeply nested paths
        let deep = "a/b/c/d/e/f/file.txt";
        let res = jail.validate(deep);
        assert!(res.is_some());
        let resolved = res.unwrap();
        assert!(resolved.to_string_lossy().contains("file.txt"));

        // Symlink-like names (should not escape - just a filename)
        assert!(jail.validate("symlink->target").is_some());
        assert!(jail.validate("..hidden").is_some());

        // Traversal with backslash (normalized to /)
        assert!(jail.validate("..\\etc\\passwd").is_none());

        std::fs::remove_dir_all(&tmp).ok();
    }
}
