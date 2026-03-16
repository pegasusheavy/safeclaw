use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;
use tracing::info;

#[link(name = "sandbox")]
extern "C" {
    fn sandbox_init(
        profile: *const c_char,
        flags: u64,
        errorbuf: *mut *mut c_char,
    ) -> c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
}

fn escape_path(p: &str) -> String {
    p.replace('"', "\\\"")
}

pub fn apply_seatbelt(data_dir: &Path, config_dir: &Path) -> std::result::Result<(), String> {
    let home = std::env::var_os("HOME")
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "/tmp".to_string());
    let data = escape_path(&data_dir.to_string_lossy().into_owned());
    let config = escape_path(&config_dir.to_string_lossy().into_owned());
    let home_esc = escape_path(&home);

    let profile = format!(
        r#"(version 1)
(deny default)
(allow file-read* file-write* (subpath "{}"))
(allow file-read* file-write* (subpath "{}"))
(allow file-read* file-write* (subpath "/tmp"))
(allow file-read* file-write* (subpath "/private/tmp"))
(allow file-read* (subpath "/usr"))
(allow file-read* (subpath "/bin"))
(allow file-read* (subpath "/sbin"))
(allow file-read* (subpath "/Library"))
(allow file-read* (subpath "/System"))
(allow file-read* (subpath "/private/etc"))
(allow file-read* (subpath "/dev"))
(allow file-read* (subpath "/opt/homebrew"))
(allow file-read* (subpath "/private/var/run"))
(allow file-read* (subpath "{}"))
(allow process-exec*)
(allow process-fork)
(allow signal)
(allow network-outbound)
(allow network-inbound)
(allow network-bind)
(allow system-socket)
(allow mach-lookup)
(allow sysctl-read)
(allow ipc-posix-shm-read*)
(allow ipc-posix-shm-write*)
"#,
        data, config, home_esc
    );

    let profile_c = CString::new(profile).map_err(|e| format!("profile CString: {e}"))?;
    let mut errorbuf: *mut c_char = std::ptr::null_mut();

    let ret = unsafe { sandbox_init(profile_c.as_ptr(), 0, &mut errorbuf) };

    if ret != 0 {
        let msg = if errorbuf.is_null() {
            "sandbox_init failed".to_string()
        } else {
            let s = unsafe { CStr::from_ptr(errorbuf).to_string_lossy().into_owned() };
            unsafe { sandbox_free_error(errorbuf) };
            s
        };
        return Err(format!("seatbelt: {msg}"));
    }

    info!("seatbelt sandbox applied");
    Ok(())
}
