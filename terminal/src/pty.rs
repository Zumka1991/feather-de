//! Pseudo-terminal with the user's shell. The master side is non-blocking and watched by
//! the event loop; the shell is started with fork+exec right away, in parallel with the
//! first frame of the window.

use std::{
    ffi::CString,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    path::Path,
};

pub struct Pty {
    master: OwnedFd,
    pub child: libc::pid_t,
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

impl Pty {
    /// Starts `$SHELL` (or /bin/bash) in `cwd` on a new pseudo-terminal of the given size
    pub fn spawn(cols: u16, rows: u16, cwd: Option<&Path>, command: Option<&[String]>) -> io::Result<Pty> {
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/bash".into());
        let argv: Vec<CString> = match command {
            Some(cmd) if !cmd.is_empty() => cmd.iter().map(|a| cstr(a)).collect(),
            _ => vec![cstr(&shell)],
        };
        let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        argv_ptrs.push(std::ptr::null());
        let cwd = cwd.map(|p| cstr(&p.to_string_lossy()));
        // The whole environment is prepared before fork: the child only calls
        // async-signal-safe functions (chdir, signal, execvpe, _exit)
        let overrides = [("TERM", "xterm-256color"), ("COLORTERM", "truecolor"), ("TERM_PROGRAM", "Feather Terminal")];
        let env: Vec<CString> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .filter(|(k, _)| !overrides.iter().any(|(o, _)| o == k) && k != "COLUMNS" && k != "LINES")
            .map(|(k, v)| cstr(&format!("{k}={v}")))
            .chain(overrides.iter().map(|(k, v)| cstr(&format!("{k}={v}"))))
            .collect();
        let mut env_ptrs: Vec<*const libc::c_char> = env.iter().map(|e| e.as_ptr()).collect();
        env_ptrs.push(std::ptr::null());

        let mut master: RawFd = -1;
        let mut size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: forkpty creates the pty pair and forks; the child touches only memory
        // prepared above and async-signal-safe calls before exec.
        let pid = unsafe { libc::forkpty(&mut master, std::ptr::null_mut(), std::ptr::null(), &mut size) };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            unsafe {
                if let Some(dir) = &cwd {
                    libc::chdir(dir.as_ptr());
                }
                // Default signal handling for the shell (the parent reaps with a SIGCHLD handler)
                libc::signal(libc::SIGCHLD, libc::SIG_DFL);
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                libc::execvpe(argv_ptrs[0], argv_ptrs.as_ptr(), env_ptrs.as_ptr());
                libc::_exit(127);
            }
        }
        // SAFETY: forkpty returned a valid master fd owned by us
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        unsafe {
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
        Ok(Pty { master, child: pid })
    }

    pub fn fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    /// Reads what is available; Ok(0) means the shell side was closed
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe { libc::read(self.fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            // Linux reports EIO on the master once the slave side has no users left
            if err.raw_os_error() == Some(libc::EIO) {
                return Ok(0);
            }
            return Err(err);
        }
        Ok(n as usize)
    }

    /// Writes all of `data`; a full pty buffer is retried briefly (input is small)
    pub fn write(&self, mut data: &[u8]) {
        let mut spins = 0;
        while !data.is_empty() {
            let n = unsafe { libc::write(self.fd(), data.as_ptr().cast(), data.len()) };
            if n > 0 {
                data = &data[n as usize..];
                continue;
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock && spins < 200 {
                spins += 1;
                std::thread::sleep(std::time::Duration::from_micros(500));
                continue;
            }
            break;
        }
    }

    pub fn resize(&self, cols: u16, rows: u16, px: (u16, u16)) {
        let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: px.0, ws_ypixel: px.1 };
        unsafe {
            libc::ioctl(self.fd(), libc::TIOCSWINSZ, &size);
        }
    }

    /// Working directory of the foreground process (for a new tab "in the same folder")
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        let fg = unsafe { libc::tcgetpgrp(self.fd()) };
        let pid = if fg > 0 { fg } else { self.child };
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    /// Name of the program in the foreground (for the tab title when the shell sets none)
    pub fn foreground_name(&self) -> Option<String> {
        let fg = unsafe { libc::tcgetpgrp(self.fd()) };
        let pid = if fg > 0 { fg } else { self.child };
        std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Closing the master sends SIGHUP to the session; make sure the shell goes away
        unsafe {
            libc::kill(self.child, libc::SIGHUP);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_a_command_on_a_pty_and_reports_its_size() {
        let cmd = vec!["/bin/sh".to_string(), "-c".to_string(), "stty size; printf 'TERM=%s' \"$TERM\"".to_string()];
        let pty = Pty::spawn(91, 27, None, Some(&cmd)).unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("27 91"), "{text:?}");
        assert!(text.contains("TERM=xterm-256color"), "{text:?}");
    }
}
