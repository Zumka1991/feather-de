//! Laptop battery from UPower. A machine without a battery starts nothing at all; on a laptop
//! a thread sleeps on UPower's signals, so the battery costs nothing between its changes.
use std::time::Duration;

use smithay::reexports::calloop::{channel, LoopHandle};
use zbus::blocking::{fdo::PropertiesProxy, Connection};
use zbus::zvariant::OwnedValue;

use crate::FeatherCompositor;

const SERVICE: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE: &str = "org.freedesktop.UPower.Device";
const LOW_PERCENT: u8 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
    pub full: bool,
    /// Seconds until empty or full, when UPower knows
    pub seconds: i64,
}

impl Battery {
    pub fn low(self) -> bool {
        self.percent <= LOW_PERCENT && !self.charging && !self.full
    }

    /// «87 % · заряжается, осталось 0:40»
    pub fn describe(self) -> String {
        let state = if self.full {
            feather_ui::i18n::tr("заряжена")
        } else if self.charging {
            feather_ui::i18n::tr("заряжается")
        } else {
            feather_ui::i18n::tr("от батареи")
        };
        let mut text = format!("{} % · {state}", self.percent);
        if self.seconds > 0 && !self.full {
            text.push_str(&format!(", {} {}:{:02}", feather_ui::i18n::tr("осталось"), self.seconds / 3600, self.seconds % 3600 / 60));
        }
        text
    }
}

fn has_battery() -> bool {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return false };
    dir.flatten().any(|supply| {
        let read = |name: &str| std::fs::read_to_string(supply.path().join(name)).unwrap_or_default();
        // "Device" scope: a mouse's or headset's own battery
        read("type").trim() == "Battery" && read("scope").trim() != "Device"
    })
}

fn parse(props: &std::collections::HashMap<String, OwnedValue>) -> Option<Battery> {
    let number = |name: &str| props.get(name).and_then(|v| f64::try_from(v).ok());
    let unsigned = |name: &str| props.get(name).and_then(|v| u32::try_from(v).ok());
    let seconds = |name: &str| props.get(name).and_then(|v| i64::try_from(v).ok()).unwrap_or(0);
    let present = props.get("IsPresent").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
    // Type 2 is a battery; the display device is 0 (unknown) on a machine with none
    if !present || unsigned("Type") != Some(2) {
        return None;
    }
    let state = unsigned("State").unwrap_or(0);
    let charging = matches!(state, 1 | 5);
    let seconds = if charging { seconds("TimeToFull") } else { seconds("TimeToEmpty") };
    Some(Battery { percent: number("Percentage")?.round().clamp(0.0, 100.0) as u8, charging, full: state == 4, seconds })
}

/// Sends the state now and after every change; returns when the bus connection ends
fn watch(connection: &Connection, updates: &channel::Sender<Option<Battery>>) -> zbus::Result<()> {
    let props = PropertiesProxy::builder(connection).destination(SERVICE)?.path(DISPLAY_DEVICE)?.build()?;
    let changes = props.receive_properties_changed()?;
    let read = || -> zbus::Result<Option<Battery>> {
        let all = props.get_all(DEVICE.try_into().map_err(zbus::Error::from)?)?;
        Ok(parse(&all))
    };
    updates.send(read()?).map_err(|_| zbus::Error::Unsupported)?;
    for _ in changes {
        updates.send(read()?).map_err(|_| zbus::Error::Unsupported)?;
    }
    Ok(())
}

pub fn start(handle: &LoopHandle<'static, FeatherCompositor>) {
    if !has_battery() {
        return;
    }
    let (updates, source) = channel::channel();
    let installed = handle.insert_source(source, |event, _, state: &mut FeatherCompositor| {
        if let channel::Event::Msg(battery) = event {
            state.battery_changed(battery);
        }
    });
    if installed.is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("feather-upower".into()).stack_size(256 * 1024).spawn(move || {
        // UPower may start after us, but a system without it should not be asked forever
        for _ in 0..3 {
            if let Ok(connection) = Connection::system() {
                let _ = watch(&connection, &updates);
            }
            std::thread::sleep(Duration::from_secs(30));
        }
    });
}

impl FeatherCompositor {
    pub fn battery_changed(&mut self, battery: Option<Battery>) {
        let before = self.shell.battery;
        if before == battery {
            return;
        }
        self.shell.battery = battery;
        self.shell.topbar_dirty = true;
        self.needs_redraw = true;
        self.mark_settings_dirty();
        if battery.is_some_and(Battery::low) && !before.is_some_and(Battery::low) {
            let body = battery.map_or(String::new(), Battery::describe);
            self.shell.notify(feather_ui::i18n::tr("Батарея почти разряжена").into_owned(), body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn props(present: bool, kind: u32, state: u32, percent: f64) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();
        map.insert("IsPresent".to_string(), OwnedValue::from(present));
        map.insert("Type".to_string(), OwnedValue::from(kind));
        map.insert("State".to_string(), OwnedValue::from(state));
        map.insert("Percentage".to_string(), OwnedValue::from(percent));
        map.insert("TimeToEmpty".to_string(), OwnedValue::from(5400i64));
        map.insert("TimeToFull".to_string(), OwnedValue::from(600i64));
        map
    }

    #[test]
    fn a_machine_without_a_battery_shows_nothing() {
        assert_eq!(parse(&props(false, 2, 2, 50.0)), None);
        assert_eq!(parse(&props(true, 0, 0, 0.0)), None);
    }

    #[test]
    fn states_and_times_come_from_the_device() {
        let b = parse(&props(true, 2, 2, 86.6)).unwrap();
        assert_eq!((b.percent, b.charging, b.full, b.seconds), (87, false, false, 5400));
        let b = parse(&props(true, 2, 1, 40.0)).unwrap();
        assert_eq!((b.charging, b.seconds), (true, 600));
        assert!(parse(&props(true, 2, 4, 100.0)).unwrap().full);
    }

    #[test]
    fn low_only_while_running_on_it() {
        assert!(parse(&props(true, 2, 2, 9.0)).unwrap().low());
        assert!(!parse(&props(true, 2, 1, 9.0)).unwrap().low());
        assert!(!parse(&props(true, 2, 2, 11.0)).unwrap().low());
    }

    struct FakeDevice {
        percent: f64,
    }

    #[zbus::interface(name = "org.freedesktop.UPower.Device")]
    impl FakeDevice {
        #[zbus(property)]
        fn percentage(&self) -> f64 {
            self.percent
        }
        #[zbus(property)]
        fn is_present(&self) -> bool {
            true
        }
        #[zbus(property, name = "Type")]
        fn kind(&self) -> u32 {
            2
        }
        #[zbus(property)]
        fn state(&self) -> u32 {
            2
        }
    }

    /// `dbus-run-session -- cargo test --release battery -- --include-ignored`
    #[test]
    #[ignore = "needs a private session bus"]
    fn follows_the_signals_of_a_real_bus() {
        let service = zbus::blocking::connection::Builder::session()
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(DISPLAY_DEVICE, FakeDevice { percent: 50.0 })
            .unwrap()
            .build()
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (updates, source) = channel::channel();
        let mut event_loop = smithay::reexports::calloop::EventLoop::<()>::try_new().unwrap();
        event_loop.handle().insert_source(source, move |event, _, _| {
            if let channel::Event::Msg(b) = event {
                let _ = tx.send(b);
            }
        }).unwrap();
        std::thread::spawn(move || {
            let client = Connection::session().unwrap();
            let _ = watch(&client, &updates);
        });
        let mut next = || {
            for _ in 0..50 {
                event_loop.dispatch(Duration::from_millis(100), &mut ()).unwrap();
                if let Ok(b) = rx.try_recv() {
                    return b;
                }
            }
            panic!("no update");
        };
        assert_eq!(next().unwrap().percent, 50);
        let iface = service.object_server().interface::<_, FakeDevice>(DISPLAY_DEVICE).unwrap();
        iface.get_mut().percent = 42.0;
        zbus::block_on(iface.get().percentage_changed(iface.signal_emitter())).unwrap();
        assert_eq!(next().unwrap().percent, 42);
    }
}
