//! Reads /proc on a background thread once a second while the window is open and sends
//! ready snapshots to the event loop; the UI thread never touches the file system.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: i32,
    pub name: String,
    pub cmd: String,
    /// Share of the whole machine (all cores = 100 %)
    pub cpu: f32,
    /// Private resident memory (RssAnon), bytes
    pub mem: u64,
    pub threads: u32,
    pub own: bool,
    pub start: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub procs: Vec<Proc>,
    pub cpu: f32,
    pub mem_total: u64,
    pub mem_used: u64,
    pub mem_cached: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// Bytes per second
    pub disk_read: f64,
    pub disk_write: f64,
    pub net_rx: f64,
    pub net_tx: f64,
    pub threads: u32,
    pub uptime: u64,
    pub cores: usize,
    pub cpu_model: String,
}

struct Prev {
    jiffies: u64,
    start: u64,
}

/// Remembers the previous counters; each `sample` reports rates since the last one.
pub struct Sampler {
    procs: HashMap<i32, Prev>,
    names: HashMap<(i32, u64), (String, String)>,
    cpu_total: u64,
    cpu_idle: u64,
    disk: (u64, u64),
    net: (u64, u64),
    at: Option<Instant>,
    uid: u32,
    page: u64,
    cores: usize,
    cpu_model: String,
}

impl Sampler {
    pub fn new() -> Self {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let cpu_model = cpuinfo
            .lines()
            .find_map(|l| l.strip_prefix("model name").and_then(|r| r.split_once(':')).map(|(_, v)| v.trim().to_string()))
            .unwrap_or_default();
        Self {
            procs: HashMap::new(),
            names: HashMap::new(),
            cpu_total: 0,
            cpu_idle: 0,
            disk: (0, 0),
            net: (0, 0),
            at: None,
            uid: unsafe { libc::getuid() },
            page: (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).max(4096) as u64,
            cores: std::thread::available_parallelism().map_or(1, |n| n.get()),
            cpu_model,
        }
    }

    pub fn sample(&mut self) -> Snapshot {
        let now = Instant::now();
        let secs = self.at.map_or(0.0, |t| now.duration_since(t).as_secs_f64());
        let first = self.at.is_none();
        self.at = Some(now);

        let (total, idle) = cpu_times();
        let d_total = total.saturating_sub(self.cpu_total);
        let cpu = if first || d_total == 0 {
            0.0
        } else {
            100.0 * (1.0 - idle.saturating_sub(self.cpu_idle) as f32 / d_total as f32)
        };
        self.cpu_total = total;
        self.cpu_idle = idle;

        let mut procs = Vec::new();
        let mut seen = HashMap::with_capacity(self.procs.len());
        let mut threads = 0;
        for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            let Some(p) = self.read_proc(pid, d_total, first) else { continue };
            threads += p.threads;
            seen.insert(pid, Prev { jiffies: 0, start: p.start });
            procs.push(p);
        }
        // `read_proc` already stored new jiffies; drop the processes that are gone
        self.procs.retain(|pid, _| seen.contains_key(pid));
        self.names.retain(|(pid, start), _| seen.get(pid).is_some_and(|p| p.start == *start));

        let mem = meminfo();
        let field = |k: &str| mem.get(k).copied().unwrap_or(0) * 1024;
        let mem_total = field("MemTotal");
        let rate = |now: u64, before: u64| if first || secs <= 0.0 { 0.0 } else { now.saturating_sub(before) as f64 / secs };
        let disk = disk_bytes();
        let net = net_bytes();
        let snap = Snapshot {
            procs,
            cpu: cpu.clamp(0.0, 100.0),
            mem_total,
            mem_used: mem_total.saturating_sub(field("MemAvailable")),
            mem_cached: field("Cached") + field("Buffers"),
            swap_total: field("SwapTotal"),
            swap_used: field("SwapTotal").saturating_sub(field("SwapFree")),
            disk_read: rate(disk.0, self.disk.0),
            disk_write: rate(disk.1, self.disk.1),
            net_rx: rate(net.0, self.net.0),
            net_tx: rate(net.1, self.net.1),
            threads,
            uptime: fs::read_to_string("/proc/uptime")
                .ok()
                .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
                .unwrap_or(0.0) as u64,
            cores: self.cores,
            cpu_model: self.cpu_model.clone(),
        };
        self.disk = disk;
        self.net = net;
        snap
    }

    fn read_proc(&mut self, pid: i32, d_total: u64, first: bool) -> Option<Proc> {
        let dir = format!("/proc/{pid}");
        let stat = fs::read_to_string(format!("{dir}/stat")).ok()?;
        // The name sits in parentheses and may itself contain spaces or ')'
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        let comm = &stat[open + 1..close];
        let f: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let ppid = num(1);
        // Kernel threads (children of kthreadd) are not programs the user can end
        if pid == 2 || ppid == 2 {
            return None;
        }
        let jiffies = num(11) + num(12);
        let start = num(19);
        let cpu = match self.procs.get(&pid) {
            Some(prev) if !first && prev.start == start && d_total > 0 => {
                100.0 * jiffies.saturating_sub(prev.jiffies) as f32 / d_total as f32
            }
            _ => 0.0,
        };
        self.procs.insert(pid, Prev { jiffies, start });
        let statm = fs::read_to_string(format!("{dir}/statm")).unwrap_or_default();
        let mut pages = statm.split_whitespace().skip(1).map(|v| v.parse::<u64>().unwrap_or(0));
        let (resident, shared) = (pages.next().unwrap_or(0), pages.next().unwrap_or(0));
        let own = fs::metadata(&dir).is_ok_and(|m| m.uid() == self.uid);
        let (name, cmd) = self
            .names
            .entry((pid, start))
            .or_insert_with(|| names(&dir, comm))
            .clone();
        Some(Proc {
            pid,
            name,
            cmd,
            cpu: cpu.clamp(0.0, 100.0),
            mem: resident.saturating_sub(shared) * self.page,
            threads: num(17) as u32,
            own,
            start,
        })
    }
}

/// Display name and full command line. `comm` is cut to 15 bytes, so a longer executable
/// name is taken from argv[0] when it starts the same way.
fn names(dir: &str, comm: &str) -> (String, String) {
    let raw = fs::read(format!("{dir}/cmdline")).unwrap_or_default();
    let args: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    let cmd = args.join(" ");
    let base = args
        .first()
        .map(|a| a.rsplit('/').next().unwrap_or(a).to_string())
        .unwrap_or_default();
    let name = if comm.len() >= 15 && base.starts_with(comm) { base } else { comm.to_string() };
    (name, cmd)
}

fn cpu_times() -> (u64, u64) {
    let stat = fs::read_to_string("/proc/stat").unwrap_or_default();
    let Some(line) = stat.lines().next().and_then(|l| l.strip_prefix("cpu ")) else {
        return (0, 0);
    };
    let v: Vec<u64> = line.split_whitespace().map(|x| x.parse().unwrap_or(0)).collect();
    // user nice system idle iowait irq softirq steal (guest time is already in user)
    let total: u64 = v.iter().take(8).sum();
    let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
    (total, idle)
}

fn meminfo() -> HashMap<String, u64> {
    fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.to_string(), v.split_whitespace().next()?.parse().ok()?))
        })
        .collect()
}

/// Bytes read and written by whole physical disks (partitions and loop/zram devices skipped)
fn disk_bytes() -> (u64, u64) {
    let stats = fs::read_to_string("/proc/diskstats").unwrap_or_default();
    let mut sum = (0, 0);
    for line in stats.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let Some(name) = f.get(2) else { continue };
        if ["loop", "ram", "zram", "dm-", "md", "sr", "fd"].iter().any(|p| name.starts_with(p))
            || !std::path::Path::new(&format!("/sys/block/{name}")).exists()
        {
            continue;
        }
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        sum.0 += num(5) * 512;
        sum.1 += num(9) * 512;
    }
    sum
}

fn net_bytes() -> (u64, u64) {
    let dev = fs::read_to_string("/proc/net/dev").unwrap_or_default();
    let mut sum = (0, 0);
    for line in dev.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else { continue };
        if iface.trim() == "lo" {
            continue;
        }
        let f: Vec<u64> = rest.split_whitespace().map(|v| v.parse().unwrap_or(0)).collect();
        sum.0 += f.first().copied().unwrap_or(0);
        sum.1 += f.get(8).copied().unwrap_or(0);
    }
    sum
}

/// Samples until the receiver is gone: a quick second sample so CPU numbers appear at once,
/// then once a second.
pub fn spawn(mut send: impl FnMut(Snapshot) -> bool + Send + 'static) {
    std::thread::Builder::new()
        .name("feather-monitor-sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::new();
            let mut delay = Duration::from_millis(300);
            loop {
                if !send(sampler.sample()) {
                    break;
                }
                std::thread::sleep(delay);
                delay = Duration::from_secs(1);
            }
        })
        .expect("поток сбора данных");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndResult {
    Sent,
    Gone,
    Denied,
}

/// SIGTERM, or SIGKILL when `force`; the caller follows a SIGTERM up with `force_if_alive`.
pub fn end(pid: i32, force: bool) -> EndResult {
    let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
    if unsafe { libc::kill(pid, sig) } == 0 {
        return EndResult::Sent;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM) => EndResult::Denied,
        _ => EndResult::Gone,
    }
}

/// Kills a process that ignored SIGTERM, but only if the PID still names the same process
pub fn force_if_alive(pid: i32, start: u64) {
    let alive = fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|s| {
        let rest = &s[s.rfind(')')? + 1..];
        rest.split_whitespace().nth(19)?.parse::<u64>().ok()
    });
    if alive == Some(start) {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_itself_with_memory_and_rates() {
        let mut s = Sampler::new();
        let first = s.sample();
        let me = std::process::id() as i32;
        let p = first.procs.iter().find(|p| p.pid == me).expect("свой процесс в списке");
        assert!(p.own && p.mem > 0 && p.threads >= 1);
        assert!(first.mem_total > first.mem_used && first.mem_used > 0);
        assert!(first.procs.iter().all(|p| p.pid != 2));
        // Burn some CPU so the second sample reports a share for us
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < Duration::from_millis(200) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
        let second = s.sample();
        let p = second.procs.iter().find(|p| p.pid == me).unwrap();
        assert!(p.cpu > 0.0 && p.cpu <= 100.0, "cpu {}", p.cpu);
        assert!((0.0..=100.0).contains(&second.cpu));
    }

    #[test]
    #[ignore]
    fn sample_cost() {
        let mut s = Sampler::new();
        s.sample();
        let t = Instant::now();
        let n = s.sample().procs.len();
        eprintln!("выборка {n} процессов: {:.2} мс", t.elapsed().as_secs_f64() * 1e3);
    }

    #[test]
    fn ends_child_and_skips_reused_pid() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id() as i32;
        let mut s = Sampler::new();
        let start = s.sample().procs.iter().find(|p| p.pid == pid).unwrap().start;
        // A wrong start time must not kill anything
        force_if_alive(pid, start + 1);
        assert!(child.try_wait().unwrap().is_none());
        assert_eq!(end(pid, false), EndResult::Sent);
        assert!(child.wait().unwrap().code().is_none());
        assert_eq!(end(pid, false), EndResult::Gone);
    }
}
