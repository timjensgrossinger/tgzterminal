pub mod connection;
pub mod event;
mod extra_constants;
mod keycodes;
mod wgl;
pub mod window;

pub use self::window::*;
pub use connection::*;
pub use event::*;

/// Convert a rust string to a windows wide string
pub fn wide_string(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Set once a fatal-error dialog has been shown. `MessageBoxW` runs its own
/// message loop, which keeps dispatching to our windows; after a panic their
/// state may be half-borrowed, and re-entering it would turn one panic into a
/// double panic (an immediate abort that takes the dialog with it). While this
/// is set, `wnd_proc` hands every message straight to `DefWindowProcW`.
pub(crate) static FATAL_ERROR_SHOWING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Appended to every fatal-error dialog; the application sets it to name its
/// log file.
static FATAL_ERROR_HINT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Text added to the end of every [`show_fatal_error`] dialog, such as where
/// the log file is.
pub fn set_fatal_error_hint(hint: String) {
    if let Ok(mut current) = FATAL_ERROR_HINT.lock() {
        *current = hint;
    }
}

/// Show a blocking, always-on-top error dialog.
///
/// Only for errors the process is about to exit over: a GUI-subsystem process
/// has no console, so without this a startup failure is a busy cursor and then
/// nothing at all. Usable from any thread, including while a window procedure
/// is unwinding. Our windows stop processing messages once this has been
/// called, so never use it for an error the application survives.
pub fn show_fatal_error(title: &str, text: &str) {
    use winapi::um::winuser::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TASKMODAL, MB_TOPMOST,
    };

    FATAL_ERROR_SHOWING.store(true, std::sync::atomic::Ordering::SeqCst);
    let hint = FATAL_ERROR_HINT
        .lock()
        .map(|hint| hint.clone())
        .unwrap_or_default();
    let text = if hint.is_empty() {
        text.to_string()
    } else {
        format!("{text}\n\n{hint}")
    };
    let title = wide_string(title);
    let text = wide_string(&text);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND | MB_TASKMODAL,
        );
    }
}

/// Returns true if we are running in an RDP session.
/// See <https://docs.microsoft.com/en-us/windows/win32/termserv/detecting-the-terminal-services-environment>
pub fn is_running_in_rdp_session() -> bool {
    use winapi::shared::minwindef::DWORD;
    use winapi::um::processthreadsapi::{GetCurrentProcessId, ProcessIdToSessionId};
    use winapi::um::winuser::{GetSystemMetrics, SM_REMOTESESSION};
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    if unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0 {
        return true;
    }

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let terminal_server =
        match hklm.open_subkey("SYSTEM\\CurrentControlSet\\Control\\Terminal Server\\") {
            Ok(k) => k,
            Err(_) => return false,
        };

    let glass_session_id: DWORD = match terminal_server.get_value("GlassSessionId") {
        Ok(sess) => sess,
        Err(_) => return false,
    };

    unsafe {
        let mut current_session = 0;
        if ProcessIdToSessionId(GetCurrentProcessId(), &mut current_session) != 0 {
            // If we're not the glass session then we're a remote session
            current_session != glass_session_id
        } else {
            false
        }
    }
}
