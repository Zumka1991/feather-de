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

/// Starts the program in a session of its own, without our controlling terminal. The kernel
/// lets a process switch VTs through its controlling terminal: apps started from the Dock kept
/// Feather's VT as theirs, Electron apps switched VT at startup, the DRM session paused and the
/// screen locked. A new session also keeps a hangup of that terminal away from them.
pub fn new_session(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid is async-signal-safe; it fails only for a group leader, which a freshly
    // forked child is not, and then the child simply keeps the old session
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        })
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

#[cfg(test)]
mod tests {
    #[test]
    fn programs_start_in_their_own_session_without_a_terminal() {
        let mut cmd = std::process::Command::new("cat");
        cmd.arg("/proc/self/stat");
        let out = super::new_session(&mut cmd).output().unwrap();
        let stat = String::from_utf8(out.stdout).unwrap();
        // Fields after "(comm)": state ppid pgrp session tty_nr
        let fields: Vec<i64> = stat.rsplit_once(')').unwrap().1.split_whitespace().skip(1).take(4).map(|f| f.parse().unwrap()).collect();
        let (pgrp, session, tty) = (fields[1], fields[2], fields[3]);
        // SAFETY: getsid(0) only reads our own session id
        let ours = unsafe { libc::getsid(0) } as i64;
        assert_ne!(session, ours);
        assert_eq!(session, pgrp, "the child leads its new session");
        assert_eq!(tty, 0, "no controlling terminal");
    }
}
