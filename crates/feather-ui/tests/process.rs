//! Signal state is per process: this test runs in its own binary, apart from the unit tests.
use feather_ui::process::{default_child_signal, hold_reaping, reap_children};
use std::process::Command;

fn sigchld_ignored(pid: u32) -> bool {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let mask = status.lines().find_map(|l| l.strip_prefix("SigIgn:")).unwrap().trim();
    u64::from_str_radix(mask, 16).unwrap() & (1 << (libc::SIGCHLD - 1)) != 0
}

#[test]
fn children_do_not_inherit_an_ignored_sigchld() {
    // The old way: the child sees SIGCHLD ignored
    unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
    let mut child = Command::new("sleep").arg("5").spawn().unwrap();
    assert!(sigchld_ignored(child.id()));
    let _ = child.kill();
    // Reaping with a handler: the child starts clean, and its output can still be read
    reap_children();
    let mut child = Command::new("sleep").arg("5").spawn().unwrap();
    assert!(!sigchld_ignored(child.id()));
    let _ = child.kill();
    // With the handler on, a hold keeps the exit status for whoever waits. Without one the
    // handler may take it first, which is why archives failed to unpack in the real window
    let hold = hold_reaping();
    let out = Command::new("sh").args(["-c", "sleep 0.2; exit 3"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    let mut child = Command::new("sh").args(["-c", "exit 4"]).spawn().unwrap();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(4));
    // A child that finishes under a hold is collected when the hold goes
    let child = Command::new("true").spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(hold);
    assert!(!std::path::Path::new(&format!("/proc/{}", child.id())).exists());
    default_child_signal();
    let out = Command::new("echo").arg("ok").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
}
