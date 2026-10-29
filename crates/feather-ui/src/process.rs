//! Child processes of Feather's apps.
//!
//! Files and Terminal start programs and never wait for them. `SIGCHLD = SIG_IGN` would reap
//! them, but an ignored signal survives `exec`: every program opened from Files inherited it,
//! and then its own `waitpid` failed with ECHILD (the installer could not read a .deb, scripts
//! and GTK apps break the same way). A handler is reset to the default by `exec`, so children
//! start clean.

/// Reaps finished children in the background, without passing anything on to them
pub fn reap_children() {
    extern "C" fn reap(_: libc::c_int) {
        // SAFETY: waitpid and errno access are async-signal-safe; errno is restored for the
        // code the signal interrupted
        unsafe {
            let errno = *libc::__errno_location();
            while libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) > 0 {}
            *libc::__errno_location() = errno;
        }
    }
    // SAFETY: a plain sigaction with a handler that only calls async-signal-safe functions
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = reap as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART | libc::SA_NOCLDSTOP;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut());
    }
}

/// Undoes an ignored SIGCHLD inherited from whoever started us (an older Files, a script), so
/// `Command::output()` can wait for its child
pub fn default_child_signal() {
    // SAFETY: restoring the default disposition of one signal
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
}
