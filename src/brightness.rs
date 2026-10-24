//! Display brightness. A laptop panel goes through logind (`SetBrightness` on our session,
//! no root, no udev rules); an external monitor through DDC/CI with `ddcutil` (VCP 0x10).
//! Both are slow (DDC/CI takes 100–300 ms a call), so a worker thread applies only the
//! latest requested value: a slider drag never queues a backlog of writes.
use crate::FeatherCompositor;
use smithay::reexports::calloop::{channel, LoopHandle};
use std::{path::PathBuf, process::Command, sync::mpsc, time::Duration};

#[derive(Clone, Debug, PartialEq)]
enum Device {
    /// /sys/class/backlight/<name>, max_brightness
    Backlight { name: String, max: u32 },
    /// ddcutil bus number, max VCP value
    Ddc { bus: u32, max: u32 },
}

pub enum Update {
    /// Current brightness 0…1, or None when no display can be controlled
    Available(Option<f32>),
}

pub struct Brightness {
    tx: mpsc::Sender<f32>,
}

impl Brightness {
    pub fn new(handle: &LoopHandle<'static, FeatherCompositor>) -> Result<Self, Box<dyn std::error::Error>> {
        let (updates, source) = channel::channel();
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg(Update::Available(value)) = event {
                if state.shell.cc.brightness != value {
                    state.shell.cc.brightness = value;
                    state.shell.overlay_dirty = true;
                    state.needs_redraw = true;
                }
            }
        })?;
        let (tx, rx) = mpsc::channel::<f32>();
        std::thread::Builder::new()
            .name("feather-brightness".into())
            .stack_size(256 * 1024)
            .spawn(move || worker(rx, updates))?;
        Ok(Self { tx })
    }

    pub fn set(&self, value: f32) {
        let _ = self.tx.send(value.clamp(0.0, 1.0));
    }
}

fn backlight() -> Option<Device> {
    let dir = std::fs::read_dir("/sys/class/backlight").ok()?;
    // Firmware/platform interfaces control the real panel better than raw ones
    let mut found: Vec<(u8, String, u32)> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let base = PathBuf::from("/sys/class/backlight").join(&name);
            let max = std::fs::read_to_string(base.join("max_brightness")).ok()?.trim().parse().ok().filter(|m| *m > 0)?;
            let kind = std::fs::read_to_string(base.join("type")).unwrap_or_default();
            let rank = match kind.trim() { "firmware" => 0, "platform" => 1, _ => 2 };
            Some((rank, name, max))
        })
        .collect();
    found.sort();
    found.into_iter().next().map(|(_, name, max)| Device::Backlight { name, max })
}

/// `ddcutil detect -t` → the first display's I²C bus
fn parse_detect(out: &str) -> Option<u32> {
    out.lines().find_map(|l| l.trim().strip_prefix("I2C bus:")).and_then(|b| b.trim().strip_prefix("/dev/i2c-")).and_then(|n| n.parse().ok())
}

/// `ddcutil -t getvcp 10` → (current, max), e.g. "VCP 10 C 70 100"
fn parse_vcp(out: &str) -> Option<(u32, u32)> {
    let f: Vec<_> = out.split_whitespace().collect();
    match f.as_slice() {
        ["VCP", _, "C", cur, max, ..] => Some((cur.parse().ok()?, max.parse().ok().filter(|m: &u32| *m > 0)?)),
        _ => None,
    }
}

fn ddc() -> Option<Device> {
    let detect = Command::new("ddcutil").args(["detect", "-t"]).output().ok()?;
    let bus = parse_detect(&String::from_utf8_lossy(&detect.stdout))?;
    let (_, max) = ddc_read(bus)?;
    Some(Device::Ddc { bus, max })
}

fn ddc_read(bus: u32) -> Option<(u32, u32)> {
    let out = Command::new("ddcutil").args(["-t", "--bus", &bus.to_string(), "getvcp", "10"]).output().ok()?;
    parse_vcp(&String::from_utf8_lossy(&out.stdout))
}

fn read(device: &Device) -> Option<f32> {
    match device {
        Device::Backlight { name, max } => {
            let v: u32 = std::fs::read_to_string(format!("/sys/class/backlight/{name}/brightness")).ok()?.trim().parse().ok()?;
            Some(v as f32 / *max as f32)
        }
        Device::Ddc { bus, .. } => ddc_read(*bus).map(|(c, m)| c as f32 / m as f32),
    }
}

/// Never fully dark: a black panel looks like a dead one
fn raw_value(value: f32, max: u32) -> u32 {
    ((value * max as f32).round() as u32).clamp((max / 100).max(1), max)
}

fn write(device: &Device, value: f32) -> bool {
    match device {
        Device::Backlight { name, max } => {
            let raw = raw_value(value, *max);
            futures_lite::future::block_on(async {
                let Ok(conn) = zbus::Connection::system().await else { return false };
                conn.call_method(
                    Some("org.freedesktop.login1"),
                    "/org/freedesktop/login1/session/auto",
                    Some("org.freedesktop.login1.Session"),
                    "SetBrightness",
                    &("backlight", name.as_str(), raw),
                )
                .await
                .is_ok()
            })
        }
        Device::Ddc { bus, max } => Command::new("ddcutil")
            .args(["--bus", &bus.to_string(), "--noverify", "setvcp", "10", &raw_value(value, *max).to_string()])
            .output()
            .is_ok_and(|o| o.status.success()),
    }
}

fn worker(rx: mpsc::Receiver<f32>, updates: channel::Sender<Update>) {
    let device = backlight().or_else(ddc);
    let _ = updates.send(Update::Available(device.as_ref().and_then(read)));
    let Some(device) = device else { return };
    while let Ok(mut value) = rx.recv() {
        // Only the latest value of a burst (a slider drag) is written
        while let Ok(next) = rx.recv_timeout(Duration::from_millis(30)) {
            value = next;
        }
        if !write(&device, value) {
            tracing::debug!("Яркость не установлена");
        }
    }
}

impl FeatherCompositor {
    pub fn set_brightness(&mut self, value: f32) {
        let value = value.clamp(0.0, 1.0);
        if let Some(b) = &self.brightness {
            b.set(value);
        }
        if self.shell.cc.brightness.is_some() {
            self.shell.cc.brightness = Some(value);
            self.shell.overlay_dirty = true;
            self.needs_redraw = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real hardware, read-only in effect: writes back the value it read
    #[test]
    #[ignore = "talks to the real display: FEATHER_HW_TEST=1 cargo test --release brightness_round_trip -- --ignored"]
    fn brightness_round_trip_on_this_machine() {
        if std::env::var_os("FEATHER_HW_TEST").is_none() {
            return eprintln!("set FEATHER_HW_TEST=1 to touch the real display");
        }
        let Some(device) = backlight().or_else(ddc) else { return eprintln!("no controllable display") };
        let value = read(&device).expect("readable");
        assert!((0.0..=1.0).contains(&value));
        let start = std::time::Instant::now();
        assert!(write(&device, value), "writes");
        eprintln!("{device:?}: {value} (write {:?})", start.elapsed());
        assert!((read(&device).unwrap() - value).abs() < 0.02);
    }

    #[test]
    fn ddcutil_output_is_parsed_and_values_stay_visible() {
        let detect = "Display 1\n   I2C bus:          /dev/i2c-4\n   DRM connector:    card1-HDMI-A-2\n";
        assert_eq!(parse_detect(detect), Some(4));
        assert_eq!(parse_detect("No displays found"), None);
        assert_eq!(parse_vcp("VCP 10 C 70 100\n"), Some((70, 100)));
        assert_eq!(parse_vcp("VCP 10 ERR"), None);
        assert_eq!(parse_vcp("VCP 10 C 5 0"), None, "max 0 is not a range");
        assert_eq!(raw_value(0.0, 100), 1, "never fully black");
        assert_eq!(raw_value(0.5, 100), 50);
        assert_eq!(raw_value(1.0, 19393), 19393);
    }
}
