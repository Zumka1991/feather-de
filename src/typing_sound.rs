//! Studio-recorded polyphonic ASMR mechanical keyboard sound engine on PipeWire.
//!
//! Uses genuine 48 kHz 16-bit condenser-microphone switch recordings (MIT-licensed studio packs
//! from `hainguyents13/mechvibes` and `yingDev/Tickeys`) embedded directly in read-only `.rodata`
//! (`typing_packs.pcm`, ~402 KiB):
//! - **0 bytes of `RssAnon` heap memory** for audio samples (`include_bytes!` stays in file-backed
//!   read-only pages, with zero runtime decoding or allocations).
//! - **0 % CPU in idle**: starts `INACTIVE` at boot, pre-warms silently on session unlock, stays
//!   hot at an XRUN-free `256/48000` (`5.33 ms`) quantum for 120 s of typing activity so ALSA
//!   never suspends mid-session, and pauses back to `0` CPU ticks after inactivity.
//! - **Natural 24-voice polyphony + sub-poll roll buffering**: real recorded `KeyA/S/D/F/J/K`,
//!   `Space`, `Enter`, `Backspace`, and `Shift` slices overlap naturally just like a real mechanical
//!   keyboard, with subtle ±1.5 % playback-rate humanization and stereo column panning.

use pipewire as pw;
use pw::spa;
use std::{
    cell::UnsafeCell,
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};

pub const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const BYTES_PER_SAMPLE: usize = 2;
const FRAME_STRIDE: usize = CHANNELS * BYTES_PER_SAMPLE;
const DEFAULT_QUANTUM_FRAMES: usize = 256;
const ALPHA_VARIANTS: usize = 6;
const MAX_VOICES: usize = 24;
const RING_CAP: usize = 64;
const RING_MASK: usize = RING_CAP - 1;
/// When 2+ keys arrive inside the same <4 ms USB poll packet or the same PipeWire audio quantum,
/// stagger by 9 ms (432 frames) so rapid finger rolls sound crisp and cleanly separated.
const SIMULTANEOUS_POLL_FRAMES: u32 = 192;
const ROLL_STAGGER_FRAMES: u32 = 432;
const MAX_ROLL_LOOKAHEAD_FRAMES: u32 = ROLL_STAGGER_FRAMES * 6;
/// Keep the PipeWire stream hot for 120 s after any keystroke, then pause to 0 % CPU.
const IDLE_PAUSE_FRAMES: u32 = SAMPLE_RATE * 120;

/// Read-only 48 kHz mono `i16` little-endian studio recordings (MIT: Mechvibes & Tickeys).
static RAW_PCM: &[u8] = include_bytes!("typing_packs.pcm");

#[derive(Clone, Copy)]
struct PackSlices {
    alphas: [(usize, usize); ALPHA_VARIANTS],
    space: (usize, usize),
    enter: (usize, usize),
    backspace: (usize, usize),
    modifier: (usize, usize),
}

const PACKS: [PackSlices; 6] = [
    // 0: Oreo (Everglide Oreo)
    PackSlices {
        alphas: [
            (0, 2784),
            (2784, 2784),
            (5568, 2784),
            (8352, 2784),
            (11136, 2784),
            (13920, 2784),
        ],
        space: (16704, 3552),
        enter: (20256, 3264),
        backspace: (23520, 3072),
        modifier: (26592, 2784),
    },
    // 1: CrystalPurple (Everglide Crystal Purple)
    PackSlices {
        alphas: [
            (29376, 2784),
            (32160, 2784),
            (34944, 2784),
            (37728, 2784),
            (40512, 2784),
            (43296, 2784),
        ],
        space: (46080, 3552),
        enter: (49632, 3264),
        backspace: (52896, 3072),
        modifier: (55968, 2784),
    },
    // 2: Topre (Topre Purple Hybrid PBT)
    PackSlices {
        alphas: [
            (58752, 3264),
            (62016, 3264),
            (65280, 3264),
            (68544, 3264),
            (71808, 3264),
            (75072, 3264),
        ],
        space: (78336, 4032),
        enter: (82368, 3744),
        backspace: (86112, 3456),
        modifier: (89568, 3072),
    },
    // 3: CherryBrown (Cherry MX Brown PBT)
    PackSlices {
        alphas: [
            (92640, 3264),
            (95904, 3264),
            (99168, 3264),
            (102432, 3264),
            (105696, 3264),
            (108960, 3264),
        ],
        space: (112224, 4032),
        enter: (116256, 3744),
        backspace: (120000, 3456),
        modifier: (123456, 3072),
    },
    // 4: CherryBlue (Cherry MX Blue PBT)
    PackSlices {
        alphas: [
            (126528, 3264),
            (129792, 3264),
            (133056, 3264),
            (136320, 3264),
            (139584, 3264),
            (142848, 3264),
        ],
        space: (146112, 4032),
        enter: (150144, 3744),
        backspace: (153888, 3456),
        modifier: (157344, 3072),
    },
    // 5: Typewriter (Vintage Mechanical Typewriter)
    PackSlices {
        alphas: [
            (160416, 3456),
            (163872, 3456),
            (167328, 3456),
            (170784, 3456),
            (174240, 3456),
            (177696, 3264),
        ],
        space: (180960, 3936),
        enter: (184896, 6480),
        backspace: (191376, 3264),
        modifier: (194640, 2976),
    },
];

/// Studio switch profiles selectable in System Settings → Keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(usize)]
pub enum TypingProfile {
    #[default]
    Oreo = 0,
    CrystalPurple = 1,
    Topre = 2,
    CherryBrown = 3,
    CherryBlue = 4,
    Typewriter = 5,
}

impl TypingProfile {
    pub const ALL: [Self; 6] = [
        Self::Oreo,
        Self::CrystalPurple,
        Self::Topre,
        Self::CherryBrown,
        Self::CherryBlue,
        Self::Typewriter,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Oreo => "oreo",
            Self::CrystalPurple => "purple",
            Self::Topre => "topre",
            Self::CherryBrown => "brown",
            Self::CherryBlue => "blue",
            Self::Typewriter => "typewriter",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Oreo => "Everglide Oreo",
            Self::CrystalPurple => "Crystal Purple",
            Self::Topre => "Topre Hybrid PBT",
            Self::CherryBrown => "Cherry MX Brown PBT",
            Self::CherryBlue => "Cherry MX Blue PBT",
            Self::Typewriter => "Печатная машинка",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "oreo" | "creamy" => Some(Self::Oreo),
            "purple" => Some(Self::CrystalPurple),
            "topre" => Some(Self::Topre),
            "brown" => Some(Self::CherryBrown),
            "blue" | "clicky" => Some(Self::CherryBlue),
            "typewriter" => Some(Self::Typewriter),
            _ => None,
        }
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    #[inline]
    fn slice(self, kind: KeyKind, variant: usize) -> (usize, usize) {
        let p = &PACKS[self as usize];
        match kind {
            KeyKind::Alpha => p.alphas[variant % ALPHA_VARIANTS],
            KeyKind::Space => p.space,
            KeyKind::Enter => p.enter,
            KeyKind::Backspace => p.backspace,
            KeyKind::Modifier => p.modifier,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum KeyKind {
    Alpha = 0,
    Space = 1,
    Enter = 2,
    Backspace = 3,
    Modifier = 4,
}

#[cfg(test)]
impl KeyKind {
    const ALL: [Self; 5] = [
        Self::Alpha,
        Self::Space,
        Self::Enter,
        Self::Backspace,
        Self::Modifier,
    ];
}

/// Maps a Linux `evdev` keycode (XKB raw minus 8) to `(KeyKind, stereo_pan)`.
/// Returns `None` for non-typing hardware buttons (volume, mute, media, brightness, power, print).
pub fn classify_evdev(evdev: u32) -> Option<(KeyKind, f32)> {
    match evdev {
        0 | 99 | 113..=116 | 142 | 163..=166 | 171..=173 | 224 | 225 | 240 => None,
        57 => Some((KeyKind::Space, 0.0)),
        28 => Some((KeyKind::Enter, 0.10)),
        96 => Some((KeyKind::Enter, 0.12)),
        14 => Some((KeyKind::Backspace, 0.11)),
        111 => Some((KeyKind::Backspace, 0.12)),
        15 => Some((KeyKind::Backspace, -0.11)),
        1 | 29 | 42 | 56 | 58 | 125 => Some((KeyKind::Modifier, -0.11)),
        54 | 97 | 100 | 126 | 127 => Some((KeyKind::Modifier, 0.10)),
        102..=110 => Some((KeyKind::Modifier, 0.11)),
        41 => Some((KeyKind::Alpha, -0.12)),
        2..=13 => {
            let col = (evdev - 2) as f32 / 11.0;
            Some((KeyKind::Alpha, -0.10 + col * 0.20))
        }
        16..=27 => {
            let col = (evdev - 16) as f32 / 11.0;
            Some((KeyKind::Alpha, -0.10 + col * 0.20))
        }
        43 => Some((KeyKind::Alpha, 0.11)),
        30..=40 => {
            let col = (evdev - 30) as f32 / 10.0;
            Some((KeyKind::Alpha, -0.09 + col * 0.18))
        }
        86 => Some((KeyKind::Alpha, -0.10)),
        44..=53 => {
            let col = (evdev - 44) as f32 / 9.0;
            Some((KeyKind::Alpha, -0.08 + col * 0.16))
        }
        59..=68 => {
            let col = (evdev - 59) as f32 / 9.0;
            Some((KeyKind::Modifier, -0.09 + col * 0.18))
        }
        87 | 88 => Some((KeyKind::Modifier, 0.09)),
        69..=83 | 98 => Some((KeyKind::Alpha, 0.12)),
        _ => Some((KeyKind::Alpha, 0.0)),
    }
}

/// Pure policy check so password fields, lock screen, and optional game muting are testable.
pub fn should_play_typing_sound(
    enabled: bool,
    volume: u8,
    mute_games: bool,
    in_game: bool,
    locked: bool,
    password_active: bool,
    capturing_shortcut: bool,
) -> bool {
    enabled
        && volume > 0
        && !locked
        && !password_active
        && !capturing_shortcut
        && !(mute_games && in_game)
}

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    #[inline]
    fn bipolar(&mut self) -> f32 {
        let u = (self.next_u64() >> 11) as f64 * (1.0 / ((1u64 << 53) as f64));
        (u * 2.0 - 1.0) as f32
    }
}

#[inline]
fn pcm_sample(sample_idx: usize) -> f32 {
    let byte = sample_idx * 2;
    if byte + 1 >= RAW_PCM.len() {
        return 0.0;
    }
    i16::from_le_bytes([RAW_PCM[byte], RAW_PCM[byte + 1]]) as f32 * (1.0 / 32_768.0)
}

#[inline]
fn slice_lerp(start: usize, len: usize, pos: f32) -> f32 {
    if pos < 0.0 {
        return 0.0;
    }
    let idx = pos as usize;
    if idx + 1 >= len {
        return 0.0;
    }
    let frac = pos - idx as f32;
    let a = pcm_sample(start + idx);
    let b = pcm_sample(start + idx + 1);
    a + (b - a) * frac
}

#[inline]
fn soft_limit_i16(x: f32) -> i16 {
    // Transparent up to ±0.85, smooth soft knee near full-scale so overlapping chords never clip
    let ax = x.abs();
    let y = if ax <= 0.80 {
        x
    } else {
        x.signum() * (0.80 + 0.20 * ((ax - 0.80) / (0.20 + (ax - 0.80))))
    };
    (y * 32_767.0).round().clamp(-32_767.0, 32_767.0) as i16
}

#[derive(Clone, Copy)]
struct Trigger {
    profile: TypingProfile,
    kind: KeyKind,
    pan: f32,
    delay_frames: u32,
    gain_mul: f32,
    stamped_at: Option<Instant>,
    clear_before: bool,
}

impl Trigger {
    const EMPTY: Self = Self {
        profile: TypingProfile::Oreo,
        kind: KeyKind::Alpha,
        pan: 0.0,
        delay_frames: 0,
        gain_mul: 1.0,
        stamped_at: None,
        clear_before: false,
    };
}

/// Wait-free single-producer (compositor main thread) / single-consumer (PipeWire RT thread)
/// ring buffer so `RT_PROCESS` never contends on a mutex and keystrokes need zero thread hops.
struct TriggerRing {
    slots: [UnsafeCell<Trigger>; RING_CAP],
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: `push` is exclusively called by `TypingSound` on the compositor thread (writing only to
// `slots[tail & RING_MASK]` and `tail`), while `pop` is exclusively called by `RtContext` on the
// PipeWire RT thread (reading `slots[head & RING_MASK]` and writing `head`). `Acquire`/`Release`
// ordering on `head`/`tail` guarantees disjoint slot access without data races.
unsafe impl Sync for TriggerRing {}

impl TriggerRing {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| UnsafeCell::new(Trigger::EMPTY)),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    fn push(&self, trig: Trigger) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= RING_CAP {
            return false;
        }
        // SAFETY: slot `tail & RING_MASK` is outside `[head, tail)` so consumer will not read it
        // until `tail` is published with `Release` below.
        unsafe {
            *self.slots[tail & RING_MASK].get() = trig;
        }
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    fn pop(&self) -> Option<Trigger> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: slot `head & RING_MASK` was fully written before `tail` was published with `Release`.
        let trig = unsafe { *self.slots[head & RING_MASK].get() };
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Some(trig)
    }

    fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) == self.tail.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    start: usize,
    len: usize,
    pos: f32,
    step: f32,
    gain_l: f32,
    gain_r: f32,
    delay_frames: u32,
    age: u32,
}

/// Real-time allocation-free 24-voice studio sample mixer.
pub struct Mixer {
    voices: [Voice; MAX_VOICES],
    alpha_cursor: usize,
    rng: Rng,
    roll_cursor_frames: u32,
    last_trigger_at: Option<Instant>,
    idle_frames: u32,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    pub fn new() -> Self {
        Self {
            voices: [Voice::default(); MAX_VOICES],
            alpha_cursor: 0,
            rng: Rng::new(0x517C_C1B7_2722_0A95),
            roll_cursor_frames: 0,
            last_trigger_at: None,
            idle_frames: 0,
        }
    }

    pub fn clear(&mut self) {
        self.voices = [Voice::default(); MAX_VOICES];
        self.roll_cursor_frames = 0;
        self.last_trigger_at = None;
        self.idle_frames = 0;
    }

    pub fn is_idle(&self) -> bool {
        self.voices.iter().all(|v| !v.active)
    }

    fn trigger(&mut self, trig: Trigger, same_cycle_followup: bool) {
        let variant = self.alpha_cursor;
        if trig.kind == KeyKind::Alpha {
            self.alpha_cursor = (self.alpha_cursor + 1) % ALPHA_VARIANTS;
        }
        let (start, len) = trig.profile.slice(trig.kind, variant);

        // Identical ±1.5 % playback-rate humanization as the WebAudio studio preview
        let step = 1.0 + self.rng.bipolar() * 0.015;
        let human_gain = (1.0 + self.rng.bipolar() * 0.035) * trig.gain_mul;

        let pan = trig.pan.clamp(-0.20, 0.20);
        let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
        let gain_l = angle.cos() * human_gain;
        let gain_r = angle.sin() * human_gain;

        let delay_frames = if let Some(at) = trig.stamped_at {
            let dt_frames = self
                .last_trigger_at
                .and_then(|prev| at.checked_duration_since(prev))
                .map_or(u32::MAX, |d| (d.as_secs_f64() * SAMPLE_RATE as f64) as u32);
            self.last_trigger_at = Some(at);

            if same_cycle_followup {
                let gap = dt_frames.max(ROLL_STAGGER_FRAMES);
                let next = (self.roll_cursor_frames + gap).min(MAX_ROLL_LOOKAHEAD_FRAMES);
                self.roll_cursor_frames = next;
                next
            } else if dt_frames < SIMULTANEOUS_POLL_FRAMES {
                let next = (self.roll_cursor_frames + ROLL_STAGGER_FRAMES).min(MAX_ROLL_LOOKAHEAD_FRAMES);
                self.roll_cursor_frames = next;
                next
            } else {
                0
            }
        } else {
            trig.delay_frames
        };

        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, v)| v.age)
                    .map_or(0, |(idx, _)| idx)
            });

        self.voices[slot] = Voice {
            active: true,
            start,
            len,
            pos: 0.0,
            step,
            gain_l,
            gain_r,
            delay_frames,
            age: 0,
        };
        self.idle_frames = 0;
    }

    pub fn render(&mut self, volume: u8, dst: &mut [u8]) -> usize {
        let n_frames = dst.len() / FRAME_STRIDE;
        self.roll_cursor_frames = self.roll_cursor_frames.saturating_sub(n_frames as u32);

        if volume == 0 || self.is_idle() {
            dst[..n_frames * FRAME_STRIDE].fill(0);
            self.idle_frames = self.idle_frames.saturating_add(n_frames as u32);
            return n_frames;
        }

        // Compensate for cos(PI/4) = 0.7071 equal-power center pan so 85–100 % matches full studio level
        let master = ((volume.min(100) as f32) / 100.0).powf(1.12) * 1.38;

        for frame_idx in 0..n_frames {
            let mut sum_l = 0.0f32;
            let mut sum_r = 0.0f32;

            for v in &mut self.voices {
                if !v.active {
                    continue;
                }
                if v.delay_frames > 0 {
                    v.delay_frames -= 1;
                    continue;
                }
                if v.pos >= (v.len.saturating_sub(1)) as f32 {
                    v.active = false;
                    continue;
                }
                let s = slice_lerp(v.start, v.len, v.pos);
                sum_l += s * v.gain_l;
                sum_r += s * v.gain_r;
                v.pos += v.step;
                v.age = v.age.saturating_add(1);
            }

            let out_l = soft_limit_i16(sum_l * master);
            let out_r = soft_limit_i16(sum_r * master);
            let base = frame_idx * FRAME_STRIDE;
            dst[base..base + 2].copy_from_slice(&out_l.to_le_bytes());
            dst[base + 2..base + 4].copy_from_slice(&out_r.to_le_bytes());
        }

        if self.is_idle() {
            self.idle_frames = self.idle_frames.saturating_add(n_frames as u32);
        } else {
            self.idle_frames = 0;
        }
        n_frames
    }
}

#[inline]
fn target_quantum_frames(requested: u64, max_frames: usize) -> usize {
    let req = requested as usize;
    if req > 0 {
        req.min(max_frames)
    } else {
        DEFAULT_QUANTUM_FRAMES.min(max_frames)
    }
}

enum ToAudio {
    Wake,
    Pause,
    IdlePause,
}

struct SharedState {
    ring: TriggerRing,
    volume: AtomicU8,
    clear_now: AtomicBool,
    has_work: AtomicBool,
    stream_active: AtomicBool,
    idle_pause_armed: AtomicBool,
}

struct RtContext {
    shared: Arc<SharedState>,
    mixer: Mixer,
    self_tx: pw::channel::Sender<ToAudio>,
}

/// Compositor-side handle. Allocates nothing and spawns no threads while disabled.
#[derive(Default)]
pub struct TypingSound {
    tx: Option<pw::channel::Sender<ToAudio>>,
    shared: Option<Arc<SharedState>>,
    failed: bool,
    held_keys: HashSet<u32>,
}

impl TypingSound {
    pub fn sync_config(&mut self, enabled: bool, volume: u8, wake_now: bool) {
        if !enabled {
            self.held_keys.clear();
            if let Some(shared) = &self.shared {
                shared.volume.store(volume.min(100), Ordering::Release);
                shared.clear_now.store(true, Ordering::Release);
                shared.has_work.store(false, Ordering::SeqCst);
                shared.idle_pause_armed.store(true, Ordering::Release);
            }
            if let Some(tx) = &self.tx {
                let _ = tx.send(ToAudio::Pause);
            }
            return;
        }
        if let Some((tx, shared)) = self.ensure_worker(volume) {
            shared.volume.store(volume.min(100), Ordering::Release);
            if wake_now {
                shared.idle_pause_armed.store(false, Ordering::Release);
                if !shared.stream_active.load(Ordering::SeqCst) {
                    let _ = tx.send(ToAudio::Wake);
                }
            }
        }
    }

    pub fn preview(&mut self, profile: TypingProfile, volume: u8) {
        let vol = if volume == 0 { 75 } else { volume.min(100) };
        let Some((tx, shared)) = self.ensure_worker(vol) else {
            return;
        };
        shared.volume.store(vol, Ordering::Release);
        // Exact natural typing phrase from the studio preview widget
        let riff = [
            (KeyKind::Alpha, -0.09, 0, 0.98),
            (KeyKind::Alpha, 0.05, 4_080, 1.00),
            (KeyKind::Alpha, -0.04, 7_920, 0.97),
            (KeyKind::Alpha, 0.08, 11_760, 1.00),
            (KeyKind::Space, 0.00, 16_800, 1.02),
            (KeyKind::Alpha, -0.06, 23_040, 0.98),
            (KeyKind::Alpha, 0.07, 26_880, 1.00),
            (KeyKind::Enter, 0.10, 32_640, 1.02),
        ];
        for (i, (kind, pan, delay_frames, gain_mul)) in riff.into_iter().enumerate() {
            shared.ring.push(Trigger {
                profile,
                kind,
                pan,
                delay_frames,
                gain_mul,
                stamped_at: None,
                clear_before: i == 0,
            });
        }
        shared.idle_pause_armed.store(false, Ordering::Release);
        shared.has_work.store(true, Ordering::SeqCst);
        if !shared.stream_active.load(Ordering::SeqCst) {
            let _ = tx.send(ToAudio::Wake);
        }
    }

    pub fn clear_held(&mut self) {
        self.held_keys.clear();
    }

    pub fn key_event(
        &mut self,
        evdev: u32,
        pressed: bool,
        allowed: bool,
        profile: TypingProfile,
        volume: u8,
    ) {
        if !pressed {
            self.held_keys.remove(&evdev);
            return;
        }
        if !self.held_keys.insert(evdev) || !allowed || volume == 0 {
            return;
        }
        let Some((kind, pan)) = classify_evdev(evdev) else {
            return;
        };
        let at = Instant::now();
        let Some((tx, shared)) = self.ensure_worker(volume) else {
            return;
        };
        shared.volume.store(volume.min(100), Ordering::Release);
        shared.ring.push(Trigger {
            profile,
            kind,
            pan,
            delay_frames: 0,
            gain_mul: 1.0,
            stamped_at: Some(at),
            clear_before: false,
        });
        shared.idle_pause_armed.store(false, Ordering::Release);
        shared.has_work.store(true, Ordering::SeqCst);
        if !shared.stream_active.load(Ordering::SeqCst) {
            let _ = tx.send(ToAudio::Wake);
        }
    }

    fn ensure_worker(&mut self, volume: u8) -> Option<(&pw::channel::Sender<ToAudio>, &Arc<SharedState>)> {
        if self.tx.is_none() && !self.failed {
            match spawn_worker(volume) {
                Some((tx, shared)) => {
                    self.tx = Some(tx);
                    self.shared = Some(shared);
                }
                None => self.failed = true,
            }
        }
        Some((self.tx.as_ref()?, self.shared.as_ref()?))
    }
}

fn spawn_worker(initial_volume: u8) -> Option<(pw::channel::Sender<ToAudio>, Arc<SharedState>)> {
    let (tx, rx) = pw::channel::channel::<ToAudio>();
    let self_tx = tx.clone();
    let shared = Arc::new(SharedState {
        ring: TriggerRing::new(),
        volume: AtomicU8::new(initial_volume.min(100)),
        clear_now: AtomicBool::new(false),
        has_work: AtomicBool::new(false),
        stream_active: AtomicBool::new(false),
        idle_pause_armed: AtomicBool::new(true),
    });
    let worker_shared = shared.clone();
    std::thread::Builder::new()
        .name("feather-keysound".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            if let Err(e) = run_worker(rx, self_tx, worker_shared) {
                tracing::debug!("Звук клавиш PipeWire недоступен: {e}");
            }
        })
        .ok()
        .map(|_| (tx, shared))
}

fn run_worker(
    rx: pw::channel::Receiver<ToAudio>,
    self_tx: pw::channel::Sender<ToAudio>,
    shared: Arc<SharedState>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    // 256/48000 (5.33 ms): identical interactive buffer quantum to Chromium WebAudio —
    // instantaneous feel under the fingers with zero ALSA XRUN crackle.
    let stream = pw::stream::StreamRc::new(
        core,
        "feather-typing-sound",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Event",
            *pw::keys::NODE_NAME => "feather-typing-sound",
            *pw::keys::NODE_DESCRIPTION => "Пёрышко — звук клавиш",
            *pw::keys::NODE_LATENCY => "256/48000",
        },
    )?;

    let rt_ctx = RtContext {
        shared: shared.clone(),
        mixer: Mixer::new(),
        self_tx,
    };

    let _listener = stream
        .add_local_listener_with_user_data(rt_ctx)
        .process(|stream, rt| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            // PipeWire allocates `maxsize` up to `clock.quantum-limit` (e.g. 8192 frames = 170 ms),
            // while each graph cycle only consumes `buffer.requested()` (256 or 512 frames).
            // Writing past `requested` causes PipeWire's audioadapter to discard the rest of the
            // sound and chop every keystroke after 5–10 ms!
            let requested = buffer.requested();
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };

            if rt.shared.clear_now.swap(false, Ordering::AcqRel) {
                rt.mixer.clear();
            }
            let mut popped = 0usize;
            while let Some(trig) = rt.shared.ring.pop() {
                if trig.clear_before {
                    rt.mixer.clear();
                    popped = 0;
                }
                rt.mixer.trigger(trig, popped > 0);
                popped += 1;
            }
            let volume = rt.shared.volume.load(Ordering::Acquire);

            let n_frames = match data.data() {
                Some(slice) => {
                    let want = target_quantum_frames(requested, slice.len() / FRAME_STRIDE);
                    rt.mixer.render(volume, &mut slice[..want * FRAME_STRIDE])
                }
                None => 0,
            };

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = FRAME_STRIDE as i32;
            *chunk.size_mut() = (n_frames * FRAME_STRIDE) as u32;

            if rt.mixer.is_idle() {
                if rt.shared.ring.is_empty() {
                    rt.shared.has_work.store(false, Ordering::SeqCst);
                }
                if rt.mixer.idle_frames >= IDLE_PAUSE_FRAMES
                    && rt.shared.ring.is_empty()
                    && !rt.shared.idle_pause_armed.swap(true, Ordering::AcqRel)
                {
                    let _ = rt.self_tx.send(ToAudio::IdlePause);
                }
            } else {
                rt.shared.has_work.store(true, Ordering::SeqCst);
            }
        })
        .register()?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::S16LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(CHANNELS as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    audio_info.set_position(position);

    let format_bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Format,
            id: spa::sys::SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        }),
    )
    .map_err(|_| pw::Error::CreationFailed)?
    .0
    .into_inner();
    let pod = spa::pod::Pod::from_bytes(&format_bytes).ok_or(pw::Error::CreationFailed)?;

    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS
            | pw::stream::StreamFlags::INACTIVE,
        &mut [pod],
    )?;

    let stream_ctl = stream.clone();
    let _rx_guard = rx.attach(mainloop.loop_(), move |msg| match msg {
        ToAudio::Wake => {
            if !shared.stream_active.load(Ordering::SeqCst) && stream_ctl.set_active(true).is_ok() {
                shared.stream_active.store(true, Ordering::SeqCst);
            }
        }
        ToAudio::Pause => {
            if shared.stream_active.swap(false, Ordering::SeqCst) {
                let _ = stream_ctl.set_active(false);
            }
        }
        ToAudio::IdlePause => {
            if shared.ring.is_empty() && !shared.has_work.load(Ordering::SeqCst) {
                if shared.stream_active.swap(false, Ordering::SeqCst) {
                    let _ = stream_ctl.set_active(false);
                }
                // Dekker re-check so a keystroke racing with IdlePause is never left paused
                if !shared.ring.is_empty() || shared.has_work.load(Ordering::SeqCst) {
                    if stream_ctl.set_active(true).is_ok() {
                        shared.stream_active.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    });

    mainloop.run();
    Ok(())
}

impl crate::FeatherCompositor {
    pub fn typing_sound_allowed(&self) -> bool {
        let k = &self.keyboard_config;
        if !k.typing_sound || k.typing_volume == 0 || self.larvis_desktop {
            return false;
        }
        let settings_wifi_pw = self.system_settings.wifi_target.is_some()
            && self
                .windows
                .iter()
                .any(|w| w.activated && w.is_visible() && w.app_id == "builtin.settings");
        let password_active = self.auth.is_open()
            || (self.shell.control_center_open
                && self.shell.cc.page == crate::control_panel::Page::Password)
            || settings_wifi_pw;
        let in_game = k.typing_mute_games
            && self
                .windows
                .iter()
                .any(|w| w.activated && w.is_visible() && self.is_game_window(w));
        should_play_typing_sound(
            k.typing_sound,
            k.typing_volume,
            k.typing_mute_games,
            in_game,
            self.lock.active,
            password_active,
            self.capturing_shortcut(),
        )
    }

    pub fn sync_typing_sound(&mut self) {
        let k = &self.keyboard_config;
        self.typing_sound
            .sync_config(k.typing_sound, k.typing_volume, false);
    }

    pub fn warm_typing_sound(&mut self) {
        let k = &self.keyboard_config;
        if k.typing_sound && k.typing_volume > 0 && !self.larvis_desktop {
            self.typing_sound
                .sync_config(true, k.typing_volume, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_six_studio_profiles_have_valid_zero_crossed_pcm_slices() {
        assert_eq!(RAW_PCM.len(), 197_616 * 2);
        for profile in TypingProfile::ALL {
            assert_eq!(TypingProfile::parse(profile.id()), Some(profile));
            for kind in KeyKind::ALL {
                for v in 0..ALPHA_VARIANTS {
                    let (start, len) = profile.slice(kind, v);
                    assert!(len >= 1000, "{profile:?} {kind:?} slice too short");
                    assert!(
                        (start + len) * 2 <= RAW_PCM.len(),
                        "{profile:?} {kind:?} out of bounds"
                    );
                    assert_eq!(pcm_sample(start), 0.0, "must start at zero without pop");
                    assert_eq!(pcm_sample(start + len - 1), 0.0, "must fade to zero");
                    let peak = (0..len)
                        .map(|i| pcm_sample(start + i).abs())
                        .fold(0.0f32, f32::max);
                    assert!(peak > 0.45, "{profile:?} {kind:?} studio recording too quiet");
                }
            }
        }
        // Legacy ID aliases map cleanly
        assert_eq!(TypingProfile::parse("creamy"), Some(TypingProfile::Oreo));
        assert_eq!(TypingProfile::parse("clicky"), Some(TypingProfile::CherryBlue));
    }

    #[test]
    fn pipewire_quantum_clamping_prevents_buffer_truncation_and_streams_full_tail() {
        // PipeWire allocates 8192-frame buffers while requesting 256 or 512 frames per cycle.
        assert_eq!(target_quantum_frames(512, 8192), 512);
        assert_eq!(target_quantum_frames(256, 8192), 256);
        assert_eq!(target_quantum_frames(0, 8192), DEFAULT_QUANTUM_FRAMES);
        assert_eq!(target_quantum_frames(1024, 512), 512);

        // A 68 ms Cherry MX Blue keystroke must span ~7 continuous 512-frame PipeWire quanta
        // with real studio body energy across quanta 0..3 rather than truncating at 10.67 ms.
        let mut mixer = Mixer::new();
        mixer.trigger(
            Trigger {
                profile: TypingProfile::CherryBlue,
                kind: KeyKind::Alpha,
                pan: 0.0,
                delay_frames: 0,
                gain_mul: 1.0,
                stamped_at: None,
                clear_before: false,
            },
            false,
        );
        let mut quantum_peaks = Vec::new();
        let mut chunk = vec![0u8; 512 * FRAME_STRIDE];
        for _ in 0..8 {
            mixer.render(100, &mut chunk);
            let pk = chunk
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
                .max()
                .unwrap_or(0);
            quantum_peaks.push(pk);
        }
        assert!(quantum_peaks[0] > 6000, "quantum 0 (0..10.7ms) missing attack: {quantum_peaks:?}");
        assert!(quantum_peaks[1] > 4000, "quantum 1 (10.7..21.3ms) missing body: {quantum_peaks:?}");
        assert!(quantum_peaks[2] > 1000, "quantum 2 (21.3..32ms) missing decay: {quantum_peaks:?}");
        assert!(mixer.is_idle(), "voice should finish cleanly after ~68 ms");
    }

    #[test]
    fn polyphonic_studio_mixer_layers_keys_and_staggers_sub_poll_rolls() {
        let mut mixer = Mixer::new();
        let now = Instant::now();
        for i in 0..3 {
            mixer.trigger(
                Trigger {
                    profile: TypingProfile::Oreo,
                    kind: KeyKind::Alpha,
                    pan: -0.08 + i as f32 * 0.08,
                    delay_frames: 0,
                    gain_mul: 1.0,
                    stamped_at: Some(now),
                    clear_before: false,
                },
                i > 0,
            );
        }
        let delays: Vec<u32> = mixer
            .voices
            .iter()
            .filter(|v| v.active)
            .map(|v| v.delay_frames)
            .collect();
        assert_eq!(delays, vec![0, ROLL_STAGGER_FRAMES, ROLL_STAGGER_FRAMES * 2]);

        let mut pcm = vec![0u8; 6000 * FRAME_STRIDE];
        mixer.render(100, &mut pcm);
        assert!(mixer.is_idle());
        assert!(pcm.chunks_exact(2).any(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs() > 8000));

        // Verify wait-free SPSC ring FIFO wrap-around
        let ring = TriggerRing::new();
        for i in 0..200u32 {
            assert!(ring.push(Trigger {
                delay_frames: i,
                ..Trigger::EMPTY
            }));
            assert_eq!(ring.pop().map(|t| t.delay_frames), Some(i));
        }
        assert!(ring.is_empty());
    }

    #[test]
    fn evdev_classification_and_silence_policy_cover_games_and_passwords() {
        assert_eq!(classify_evdev(57), Some((KeyKind::Space, 0.0)));
        assert_eq!(classify_evdev(28).map(|v| v.0), Some(KeyKind::Enter));
        assert_eq!(classify_evdev(14).map(|v| v.0), Some(KeyKind::Backspace));
        assert_eq!(classify_evdev(42).map(|v| v.0), Some(KeyKind::Modifier));
        assert_eq!(classify_evdev(114), None);
        assert_eq!(classify_evdev(115), None);
        assert_eq!(classify_evdev(99), None);

        assert!(should_play_typing_sound(true, 65, false, true, false, false, false));
        assert!(!should_play_typing_sound(true, 65, true, true, false, false, false));
        assert!(should_play_typing_sound(true, 65, true, false, false, false, false));
        assert!(!should_play_typing_sound(true, 65, false, false, true, false, false));
        assert!(!should_play_typing_sound(true, 65, false, false, false, true, false));
        assert!(!should_play_typing_sound(true, 65, false, false, false, false, true));
        assert!(!should_play_typing_sound(true, 0, false, false, false, false, false));
    }
}
