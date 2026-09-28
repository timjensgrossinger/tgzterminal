use std::sync::atomic::{AtomicBool, Ordering};

/// Set when the application decides that the accelerated rendering path
/// is unusable (eg: the machine has no usable OpenGL driver) and that
/// windows should be created using the bundled software renderer.
/// This persists for the remainder of the process; any window created
/// after it is set will use software rendering.
static FORCE_SWRAST: AtomicBool = AtomicBool::new(false);

/// Force subsequent window creation to use the software renderer.
/// This is used as a fallback when the accelerated renderer fails to
/// produce a working window.
pub fn set_force_swrast(force: bool) {
    log::info!("set_force_swrast({})", force);
    FORCE_SWRAST.store(force, Ordering::SeqCst);
}

/// Ask whether window creation should use the software renderer
/// (either forced as a fallback after accelerated rendering failed,
/// or explicitly configured by the user).
pub fn force_swrast() -> bool {
    FORCE_SWRAST.load(Ordering::SeqCst)
}

pub(crate) fn prefer_swrast() -> bool {
    #[cfg(windows)]
    {
        if crate::os::windows::is_running_in_rdp_session() {
            // Using OpenGL in RDP has problematic behavior upon
            // disconnect, so we force the use of software rendering.
            log::trace!("Running in an RDP session, use SWRAST");
            return true;
        }
    }
    force_swrast() || config::configuration().front_end == config::FrontEndSelection::Software
}
