//! Pointer settings inside the labwc kiosk session (session/start-feather-de.sh): there
//! labwc owns libinput and draws the cursor, so Feather writes labwc's config (the kiosk
//! template plus a <libinput> section and an `environment` file) and asks it to reload.
//! File I/O and the signal happen on a short worker thread.
use crate::settings::Pointer;
use std::path::PathBuf;

const TEMPLATE: &str = include_str!("../session/kiosk-config/rc.xml");

/// Where start-feather-de.sh points labwc (`labwc -C`).
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| feather_ui::fileops::home().join(".config"))
        .join("feather-de/labwc")
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

pub fn rc_xml(p: &Pointer) -> String {
    let device = |category: &str, natural: bool| {
        format!(
            "    <device category=\"{category}\">\n      <naturalScroll>{}</naturalScroll>\n      <leftHanded>{}</leftHanded>\n      <pointerSpeed>{:.2}</pointerSpeed>\n      <tap>{}</tap>\n    </device>\n",
            yes(natural),
            yes(p.left_handed),
            p.speed.clamp(-1.0, 1.0),
            yes(p.tap_to_click)
        )
    };
    let libinput = format!(
        "  <!-- Written by Пёрышко: System Settings → Мышь и трекпад -->\n  <libinput>\n{}{}  </libinput>\n",
        device("non-touch", p.natural_mouse),
        device("touchpad", p.natural_touchpad)
    );
    match TEMPLATE.rfind("</labwc_config>") {
        Some(end) => format!("{}{libinput}{}", &TEMPLATE[..end], &TEMPLATE[end..]),
        None => TEMPLATE.to_owned(),
    }
}

pub fn environment(p: &Pointer) -> String {
    let theme = crate::cursor::theme_name(p.cursor_theme.as_deref());
    // labwc draws the cursor at output scale 1: the logical size times the UI scale
    format!("XCURSOR_THEME={theme}\nXCURSOR_SIZE={}\n", p.cursor_size)
}

/// The labwc that hosts us, if it reads its config from [`config_dir`].
pub fn host_pid() -> Option<i32> {
    let pid: i32 = std::env::var("LABWC_PID").ok()?.parse().ok()?;
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let dir = config_dir();
    let ours = cmdline.split(|b| *b == 0).any(|arg| std::path::Path::new(std::str::from_utf8(arg).unwrap_or("")) == dir);
    ours.then_some(pid)
}

/// Writes the config and makes labwc reload it; `cursor_scale` turns the logical cursor
/// size into labwc's pixels.
pub fn apply(p: Pointer, cursor_scale: f64) {
    let _ = std::thread::Builder::new().name("feather-labwc".into()).spawn(move || {
        let Some(pid) = host_pid() else { return };
        let dir = config_dir();
        let mut scaled = p.clone();
        scaled.cursor_size = (p.cursor_size as f64 * cursor_scale).round() as u32;
        let write = |name: &str, text: String| {
            let tmp = dir.join(format!(".{name}.tmp"));
            std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, dir.join(name)))
        };
        if std::fs::create_dir_all(&dir).is_err()
            || write("rc.xml", rc_xml(&p)).is_err()
            || write("environment", environment(&scaled)).is_err()
        {
            tracing::warn!("labwc: не удалось записать настройки мыши");
            return;
        }
        unsafe { libc::kill(pid, libc::SIGHUP) };
        tracing::info!("labwc: настройки мыши и курсора применены");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn libinput_section_inside_the_template() {
        let p = Pointer { speed: -0.5, natural_mouse: true, tap_to_click: false, ..Default::default() };
        let xml = rc_xml(&p);
        let lib = xml.find("<libinput>").unwrap();
        assert!(lib < xml.rfind("</labwc_config>").unwrap());
        assert!(xml.contains("<device category=\"non-touch\">\n      <naturalScroll>yes</naturalScroll>"));
        assert!(xml.contains("<device category=\"touchpad\">\n      <naturalScroll>yes</naturalScroll>"));
        assert!(xml.contains("<pointerSpeed>-0.50</pointerSpeed>") && xml.contains("<tap>no</tap>"));
        assert!(xml.contains("C-A-BackSpace"), "kiosk template kept");
        let env = environment(&Pointer { cursor_theme: Some("Adwaita".into()), cursor_size: 48, ..Default::default() });
        assert_eq!(env, "XCURSOR_THEME=Adwaita\nXCURSOR_SIZE=48\n");
    }
}
