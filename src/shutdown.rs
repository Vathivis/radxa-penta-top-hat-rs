use std::io;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};

const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
const SIG_ERR: usize = usize::MAX;

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn signal(signum: c_int, handler: usize) -> usize;
}

pub fn install_signal_handlers() -> io::Result<()> {
    install_signal_handler(SIGINT)?;
    install_signal_handler(SIGTERM)
}

pub fn requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

fn install_signal_handler(signum: c_int) -> io::Result<()> {
    let previous = unsafe {
        // SAFETY: `handle_signal` has C ABI, only stores to an atomic flag, and
        // remains valid for the full process lifetime.
        signal(signum, handle_signal as *const () as usize)
    };

    if previous == SIG_ERR {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

extern "C" fn handle_signal(_signum: c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}
