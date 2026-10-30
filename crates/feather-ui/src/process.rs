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

/// Sessions `new_session` gave the programs we started: leader pid and its start time
static SESSIONS: std::sync::Mutex<Vec<(i32, Option<u64>)>> = std::sync::Mutex::new(Vec::new());

/// Remembers the session of a child started with `new_session`, for `end_sessions`
pub fn remember_session(child: &std::process::Child) {
    let pid = child.id() as i32;
    let mut sessions = SESSIONS.lock().unwrap();
    // A long session launches many programs; the oldest entries are long gone
    if sessions.len() >= 4096 {
        sessions.drain(..1024);
    }
    sessions.push((pid, stat(pid).map(|s| s.start)));
}

struct Stat {
    zombie: bool,
    session: i32,
    start: u64,
}

fn stat(pid: i32) -> Option<Stat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after "(comm)": state ppid pgrp session … starttime (the 22nd field)
    let fields: Vec<&str> = text.rsplit_once(')')?.1.split_whitespace().collect();
    Some(Stat { zombie: *fields.first()? == "Z", session: fields.get(3)?.parse().ok()?, start: fields.get(19)?.parse().ok()? })
}

/// Live processes (not ourselves) in one of `sessions`
fn session_members(sessions: &[i32]) -> Vec<i32> {
    let me = std::process::id() as i32;
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&pid| pid != me && stat(pid).is_some_and(|s| !s.zombie && sessions.contains(&s.session)))
        .collect()
}

/// On logout: the programs we started live in sessions of their own, so no hangup of our
/// terminal reaches them any more. Without this, Telegram or a VPN client kept running
/// without a display, and the next login's launch only handed its window to that instance.
/// SIGTERM, then SIGKILL for whatever is left after `grace`. A leader pid that now belongs
/// to another process is skipped: while any member of our session lived, the kernel would
/// not have reused its number. Returns how many processes were asked to quit.
pub fn end_sessions(grace: std::time::Duration) -> usize {
    let sessions: Vec<i32> = std::mem::take(&mut *SESSIONS.lock().unwrap())
        .into_iter()
        .filter(|&(sid, start)| stat(sid).is_none_or(|s| Some(s.start) == start))
        .map(|(sid, _)| sid)
        .collect();
    if sessions.is_empty() {
        return 0;
    }
    let first = session_members(&sessions);
    for &pid in &first {
        // SAFETY: a signal to a process of a session we created
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let deadline = std::time::Instant::now() + grace;
    loop {
        let left = session_members(&sessions);
        if left.is_empty() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            for pid in left {
                // SAFETY: as above; it ignored SIGTERM
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    first.len()
}

/// Session of a live process, to tell whose a window is
pub fn session_of(pid: i32) -> Option<i32> {
    stat(pid).filter(|s| !s.zombie).map(|s| s.session)
}

/// What each live process of a session waits on: "pid comm state wchan". For a launch that
/// hangs without a window, wchan names the place in the kernel (FUSE request_wait_answer,
/// a poll on a D-Bus socket, a futex), so the log says where it is stuck.
pub fn describe_session(sid: i32) -> Vec<String> {
    session_members(&[sid])
        .into_iter()
        .take(16)
        .filter_map(|pid| {
            let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (head, rest) = text.rsplit_once(')')?;
            let comm = head.split_once('(')?.1;
            let state = rest.split_whitespace().next()?;
            let wchan = std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
            Some(format!("{pid} {comm} {state} {}", if wchan.is_empty() { "-" } else { wchan.trim() }))
        })
        .collect()
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

    #[test]
    fn logout_ends_the_sessions_of_started_programs() {
        // A leader with a background child, as `sh -c` leaves an app behind
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "sleep 30 & exec sleep 30"]);
        let mut child = super::new_session(&mut cmd).spawn().unwrap();
        super::remember_session(&child);
        let sid = child.id() as i32;
        let start = std::time::Instant::now();
        while super::session_members(&[sid]).len() < 2 {
            assert!(start.elapsed().as_secs() < 5, "both sleeps start");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(super::end_sessions(std::time::Duration::from_secs(2)), 2);
        let _ = child.wait();
        assert!(super::session_members(&[sid]).is_empty(), "nothing of the session survives");
        // Forgotten afterwards: a second logout has nothing to end
        assert_eq!(super::end_sessions(std::time::Duration::from_secs(1)), 0);
    }
}
