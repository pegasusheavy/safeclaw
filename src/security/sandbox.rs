use std::path::Path;
use tracing::{info, warn};

#[derive(Debug, Default)]
pub struct SandboxStatus {
    pub no_new_privs: bool,
    pub capabilities_dropped: bool,
    pub landlock: bool,
    pub seccomp: bool,
    pub seatbelt: bool,
}

impl SandboxStatus {
    pub fn active_layers(&self) -> usize {
        [
            self.no_new_privs,
            self.capabilities_dropped,
            self.landlock,
            self.seccomp,
            self.seatbelt,
        ]
        .iter()
        .filter(|&&b| b)
        .count()
    }
}

#[cfg(target_os = "linux")]
fn set_no_new_privs() -> std::result::Result<(), String> {
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret == 0 {
        Ok(())
    } else {
        Err(format!(
            "prctl(PR_SET_NO_NEW_PRIVS) failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

pub fn apply_sandbox(data_dir: &Path, config_dir: &Path) -> SandboxStatus {
    let mut status = SandboxStatus::default();

    #[cfg(target_os = "linux")]
    {
        if let Err(e) = set_no_new_privs() {
            warn!(%e, "no_new_privs failed");
        } else {
            status.no_new_privs = true;
        }

        if let Err(e) = super::drop_capabilities() {
            warn!(%e, "drop_capabilities failed");
        } else {
            status.capabilities_dropped = true;
        }

        if let Err(e) = super::apply_landlock(data_dir, config_dir) {
            warn!(%e, "landlock failed");
        } else {
            status.landlock = true;
        }

        if let Err(e) = super::seccomp::apply_seccomp() {
            warn!(%e, "seccomp failed");
        } else {
            status.seccomp = true;
        }

        info!(
            layers = status.active_layers(),
            "kernel sandbox applied (linux)"
        );
    }

    #[cfg(target_os = "macos")]
    {
        if let Err(e) = super::seatbelt::apply_seatbelt(data_dir, config_dir) {
            warn!(%e, "seatbelt failed");
        } else {
            status.seatbelt = true;
        }

        info!(
            layers = status.active_layers(),
            "kernel sandbox applied (macos)"
        );
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        info!("no kernel sandbox available on this platform; app-level isolation only");
    }

    status
}
