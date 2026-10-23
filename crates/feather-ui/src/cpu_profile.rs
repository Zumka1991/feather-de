//! Optional CPU raster diagnostics; disabled without FEATHER_PROFILE_UI.
use std::{cell::Cell, sync::LazyLock, time::Instant};

static ENABLED: LazyLock<bool> = LazyLock::new(|| std::env::var_os("FEATHER_PROFILE_UI").is_some());
thread_local! {
    static PARTS: Cell<[f64; 6]> = const { Cell::new([0.0; 6]) };
}

pub enum Part {
    Fill,
    Stroke,
    Frosted,
    Text,
    Metrics,
    Icon,
}
fn thread_ms() -> f64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Only when explicitly profiling. This clock excludes time that the OS
    // scheduled another process, unlike wall-clock first-frame latency.
    unsafe {
        libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t);
    }
    t.tv_sec as f64 * 1000.0 + t.tv_nsec as f64 / 1_000_000.0
}

pub struct Span(Option<(f64, usize)>);
impl Span {
    pub fn new(part: Part) -> Self {
        Self((*ENABLED).then(|| (thread_ms(), part as usize)))
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some((start, part)) = self.0 {
            PARTS.with(|p| {
                let mut v = p.get();
                v[part] += thread_ms() - start;
                p.set(v);
            });
        }
    }
}

pub struct Batch(Option<(Instant, f64)>);
impl Batch {
    pub fn new() -> Self {
        if *ENABLED {
            PARTS.with(|p| p.set([0.0; 6]));
        }
        Self((*ENABLED).then(|| (Instant::now(), thread_ms())))
    }
    pub fn step(&mut self, name: &str) {
        if let Some((start, cpu)) = self.0.as_mut() {
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            let work = thread_ms() - *cpu;
            let parts = PARTS.with(|p| p.replace([0.0; 6]));
            if elapsed >= 0.05 {
                eprintln!("[feather-ui] {name}: wall {elapsed:.3} мс, CPU {work:.3} мс | fill {:.3}, stroke {:.3}, frosted {:.3}, text {:.3}, metrics {:.3}, icon {:.3}", parts[0], parts[1], parts[2], parts[3], parts[4], parts[5]);
            }
            *start = Instant::now();
            *cpu = thread_ms();
        }
    }
}
