//! Opt-in end-to-end checks. Events are injected into this compositor only.
use crate::{
    shell::{DockMenuAction, DockTarget, ShellClickAction, R},
    FeatherCompositor,
};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use std::{cell::Cell, rc::Rc, time::Duration};

fn at(
    state: &FeatherCompositor,
    ms: u64,
    mut f: impl FnMut(&mut FeatherCompositor) + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    state.loop_handle.insert_source(
        Timer::from_duration(Duration::from_millis(ms)),
        move |_, _, state| {
            f(state);
            TimeoutAction::Drop
        },
    )?;
    Ok(())
}

fn tap(st:&mut FeatherCompositor,key:u32){st.debug_keyboard_key(key,true);st.debug_keyboard_key(key,false);}
fn chord(st:&mut FeatherCompositor,modifier:u32,key:u32){st.debug_keyboard_key(modifier,true);tap(st,key);st.debug_keyboard_key(modifier,false);}

/// Fake socket helper for the nested polkit check: accepts only "feather".
fn polkit_fake_helper(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let _ = std::fs::remove_file(path);
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let Ok(clone) = stream.try_clone() else { continue };
            let mut reader = BufReader::new(clone);
            let mut w = stream;
            let mut line = String::new();
            for _ in 0..2 {
                line.clear();
                let _ = reader.read_line(&mut line);
            }
            let _ = writeln!(w, "PAM_PROMPT_ECHO_OFF Password: ");
            line.clear();
            let _ = reader.read_line(&mut line);
            let ok = line.trim_end() == "feather";
            // PAM's failure delay, so the "checking" state is visible.
            std::thread::sleep(Duration::from_millis(if ok { 200 } else { 700 }));
            let _ = writeln!(w, "{}", if ok { "SUCCESS" } else { "FAILURE" });
        }
    });
    Ok(())
}

pub fn install(state: &mut FeatherCompositor) -> Result<(), Box<dyn std::error::Error>> {
    crate::monitors::install_checks(state)?;
    if std::env::var_os("FEATHER_DEBUG_SWITCHER").is_some() {
        if state.backend.is_drm() || !state.debug_input_only { return Err("switcher check requires isolated windowed input".into()); }
        fn fixture(s: &mut FeatherCompositor, specs: &[(&str,u32,bool)]) -> Vec<u64> {
            s.release_all_input();s.shell.switcher=None;s.shell.close_menus();s.windows.clear();
            s.spaces=crate::spaces::Spaces {current:0,count:2};
            let mut ids=Vec::new();
            for (i,(app,space,minimized)) in specs.iter().enumerate() {
                let id=s.next_win_id;s.next_win_id+=1;
                let mut w=crate::window::ManagedWindow::new_builtin(id,crate::apps::BuiltinWindowKind::QuickNotes,120+i as i32*80,100+i as i32*50);
                w.app_id=(*app).into();w.title=format!("Switcher fixture {app} #{id}");w.space=*space;w.minimized=*minimized;
                s.windows.push(w);ids.push(id);
            }
            s.apply_space_visibility();s.refresh_focus();s.needs_redraw=true;ids
        }
        fn front(s: &FeatherCompositor) -> u64 { s.windows.iter().rev().find(|w|w.is_visible()).unwrap().id }
        fn begin(s: &mut FeatherCompositor) { s.debug_keyboard_key(56,true);tap(s,15); }
        fn finish(s: &mut FeatherCompositor) { s.debug_keyboard_key(56,false); }
        at(state,1200,|s| {
            let ids=fixture(s,&[("builtin.about",0,false),("builtin.monitor",0,false),("builtin.settings",1,false)]);
            assert_eq!(front(s),ids[1]);
            begin(s);finish(s);assert_eq!(front(s),ids[2],"current app must anchor selection ahead of a hidden desktop's top window");
            begin(s);finish(s);assert_eq!(front(s),ids[1],"quick Alt+Tab returns to the previous app");
            s.debug_keyboard_key(42,true);begin(s);finish(s);s.debug_keyboard_key(42,false);
            assert_eq!(front(s),ids[0],"Shift before Alt must reverse even with Alt+Shift layout switching");
            // Same chord in the opposite physical order: XKB consumes Shift instead.
            let reverse_ids=fixture(s,&[("builtin.about",0,false),("builtin.monitor",0,false),("builtin.settings",1,false)]);
            s.debug_keyboard_key(56,true);s.debug_keyboard_key(42,true);tap(s,15);
            s.debug_keyboard_key(42,false);
            assert!(s.shell.switcher.is_some(),"releasing Shift must not commit while Alt is held");
            finish(s);assert_eq!(front(s),reverse_ids[0],"Alt before Shift must also reverse");
            eprintln!("[feather-switcher-check] current desktop/MRU/forward/reverse/both modifier orders: ok");
        })?;
        at(state,2000,|s| {
            let ids=fixture(s,&[("builtin.about",0,false),("builtin.monitor",0,false)]);
            for n in 0..32 {begin(s);finish(s);assert_eq!(front(s),ids[n%2],"rapid switching must not drift");}
            assert!(s.shell.switcher.is_none());
            s.debug_keyboard_key(56,true);s.debug_keyboard_key(15,true);s.debug_keyboard_key(56,false);
            assert!(s.shell.switcher.is_none());assert!(!s.shell.switcher_keys.is_empty());
            let selected=front(s);s.debug_keyboard_key(15,false);assert!(s.shell.switcher_keys.is_empty());assert_eq!(front(s),selected);
            eprintln!("[feather-switcher-check] 32 rapid alternations/Alt-before-Tab release: ok");
        })?;
        at(state,2040,|s| { begin(s); })?;
        at(state,2060,|s| { assert!(!s.shell.switcher.as_ref().unwrap().visible,"a previous invocation's timer must not reveal this panel early"); })?;
        at(state,2120,|s| {
            assert!(s.shell.switcher.as_ref().unwrap().visible);tap(s,1);finish(s);assert!(s.shell.switcher.is_none());
            eprintln!("[feather-switcher-check] timer ownership/cancel: ok");
        })?;
        at(state,2600,|s| {
            let ids=fixture(s,&[("fixture.unknown",0,false),("fixture.unknown",0,false),("builtin.monitor",0,false)]);
            begin(s);s.windows.retain(|w|w.id!=ids[0]);s.refresh_focus();finish(s);
            assert_eq!(front(s),ids[1],"closing an unknown app's first window must not invalidate the remaining selected window");
            eprintln!("[feather-switcher-check] unknown app identity after close: ok");
        })?;
        at(state,3200,|s| {
            let ids=fixture(s,&[("builtin.about",0,false),("builtin.monitor",0,false)]);
            begin(s);s.windows.retain(|w|w.id!=ids[0]);s.refresh_focus();finish(s);
            assert_eq!(s.windows.len(),1,"a closed selection must not relaunch its application");assert_eq!(front(s),ids[1]);
            eprintln!("[feather-switcher-check] closed selection does not relaunch: ok");
        })?;
        at(state,3800,|s| {
            let ids=fixture(s,&[("builtin.about",0,false),("builtin.monitor",0,false)]);
            begin(s);s.release_all_input();assert!(s.shell.switcher.is_none(),"losing host focus must cancel the switcher");
            tap(s,30);assert_eq!(front(s),ids[1],"an unrelated key must not commit a stale switcher");
            eprintln!("[feather-switcher-check] focus loss/unrelated key: ok");
        })?;
        at(state,4500,|s| {
            let ids=fixture(s,&[("builtin.about",1,true),("builtin.monitor",0,false)]);
            begin(s);finish(s);assert_eq!(front(s),ids[0],"a minimized window on another desktop must become visible");
            assert!(!s.windows.iter().find(|w|w.id==ids[0]).unwrap().minimized);
            eprintln!("[feather-switcher-check] minimized window on another desktop: ok");
        })?;
        at(state,5200,|s| {
            fixture(s,&[("builtin.about",0,false),("builtin.monitor",0,false),("builtin.settings",0,false)]);
            begin(s);
        })?;
        at(state,5600,|s| {
            assert!(s.shell.switcher.as_ref().unwrap().visible);
            eprintln!("[feather-switcher-check] complete: ok");
        })?;
        at(state,6500,|s| {tap(s,1);finish(s);})?;
    }
    if std::env::var_os("FEATHER_DEBUG_SYSTEM_SOUNDS").is_some() {
        use crate::{sounds::{Effect,Tone},system_settings::{Action as A,Page}};
        if state.backend.is_drm() || !state.debug_input_only {return Err("sounds check requires isolated windowed input".into());}
        fn click(s:&mut FeatherCompositor,predicate:impl Fn(&A)->bool) {
            let w=s.windows.iter().find(|w|w.app_id=="builtin.settings").unwrap();
            let (width,height)=(w.width as f32,w.height as f32);
            let (_,y)=s.system_settings.debug_point(width,height,&s.settings_context(),&predicate).unwrap();
            s.system_settings.scroll+=(y-height*0.6).max(0.0);
            let (x,y)=s.system_settings.debug_point(width,height,&s.settings_context(),&predicate).unwrap();
            let w=s.windows.iter().find(|w|w.app_id=="builtin.settings").unwrap();
            let (gx,gy)=(w.x as f64+x as f64,(w.y+w.titlebar_h()) as f64+y as f64);
            assert!(y>12.0 && y<height-20.0);
            s.debug_pointer_motion(gx,gy);s.debug_pointer_button(0x110,true);s.debug_pointer_button(0x110,false);
        }
        at(state,1000,|s|s.apply_system_setting(A::Page(Page::Sound)))?;
        at(state,1600,|s| {click(s,|a|matches!(a,A::AlertTone));assert_eq!(s.sound_preferences.tone,Tone::Glass);})?;
        at(state,2200,|s| {click(s,|a|matches!(a,A::AlertVolume(75)));assert_eq!(s.sound_preferences.volume,75);})?;
        at(state,2800,|s| {
            click(s,|a|matches!(a,A::AlertSounds));assert!(!s.sound_preferences.alerts);
            assert!(!s.play_effect(Effect::Alert(Tone::Glass),false));
            eprintln!("[feather-system-sounds-check] real clicks/tone/volume/disabled alerts: ok");
        })?;
        at(state,3400,|s|click(s,|a|matches!(a,A::PreviewAlert)))?;
        at(state,4000,|s| {
            click(s,|a|matches!(a,A::InterfaceSounds));assert!(!s.play_effect(Effect::Screenshot,false));
            click(s,|a|matches!(a,A::InterfaceSounds));assert!(s.play_effect(Effect::Screenshot,false));
        })?;
        at(state,4600,|s| {
            click(s,|a|matches!(a,A::VolumeFeedback));assert!(s.sound_preferences.volume_feedback);
            let audio=s.shell.cc.audio.clone();assert!(audio.current().is_some());s.show_audio_feedback(&audio);
        })?;
        at(state,5200,|s| {
            s.shell.dnd_enabled=true;
            for effect in [Effect::Screenshot,Effect::Volume,Effect::Alert(Tone::Glass)] {assert!(!s.play_effect(effect,false));}
            assert!(s.play_effect(Effect::Alert(Tone::Glass),true));s.shell.dnd_enabled=false;
        })?;
        at(state,5800,|s| {
            s.lock.active=true;assert!(!s.play_effect(Effect::Alert(Tone::Glass),true));s.lock.active=false;
            s.sound_preferences.volume=0;assert!(!s.play_effect(Effect::Alert(Tone::Glass),true));s.sound_preferences.volume=75;
            s.apply_system_setting(A::AlertSounds);assert!(s.sound_preferences.alerts);
            eprintln!("[feather-system-sounds-check] preview/interface/volume feedback/DND/lock/zero volume: ok");
        })?;
        at(state,8500,|s| {
            s.system_settings.scroll=0.0;s.mark_settings_dirty();s.needs_redraw=true;
            eprintln!("[feather-system-sounds-check] complete: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_SYSTEM_SOUNDS_RESTORE").is_some() {
        if state.backend.is_drm() || !state.debug_input_only {return Err("sounds restore requires isolated windowed input".into());}
        at(state,1200,|s| {
            assert_eq!(s.sound_preferences,crate::sounds::Preferences {alerts:true,tone:crate::sounds::Tone::Glass,volume:75,interface:true,volume_feedback:true});
            s.apply_system_setting(crate::system_settings::Action::Page(crate::system_settings::Page::Sound));
            eprintln!("[feather-system-sounds-check] restored: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_AUDIO_SETTINGS").is_some() {
        use crate::{controls::Control as C, system_settings::{Action as A, Page}};
        if state.backend.is_drm() || !state.debug_input_only { return Err("audio settings check requires isolated windowed input".into()); }
        fn click(s: &mut FeatherCompositor, predicate: impl Fn(&A) -> bool) {
            let w = s.windows.iter().find(|w| w.app_id=="builtin.settings").unwrap();
            let (width,height)=(w.width as f32,w.height as f32);
            let (_,y)=s.system_settings.debug_point(width,height,&s.settings_context(),&predicate).unwrap();
            s.system_settings.scroll += (y-height*0.6).max(0.0);
            let (x,y)=s.system_settings.debug_point(width,height,&s.settings_context(),&predicate).unwrap();
            let w=s.windows.iter().find(|w| w.app_id=="builtin.settings").unwrap();
            let (gx,gy)=(w.x as f64+x as f64,(w.y+w.titlebar_h()) as f64+y as f64);
            assert!(y>12.0 && y<height-20.0);
            s.debug_pointer_motion(gx,gy);
            s.debug_pointer_button(0x110,true);s.debug_pointer_button(0x110,false);
        }
        at(state,1200,|s| {
            assert_eq!(s.shell.cc.audio.sources.len(),2,"output monitors are excluded");
            s.apply_system_setting(A::Page(Page::Sound));
        })?;
        at(state,1700,|s| { click(s,|a| matches!(a,A::Audio(C::Input(name)) if name=="usb mic")); })?;
        at(state,2400,|s| {
            assert_eq!(s.shell.cc.audio.default_source,"usb mic");
            click(s,|a| matches!(a,A::Audio(C::InputVolume { .. })));
        })?;
        at(state,3100,|s| {
            assert!((s.shell.cc.audio.current_source().unwrap().volume()-0.5).abs()<0.001);
            click(s,|a| matches!(a,A::Audio(C::InputMute { .. })));
            let w=s.windows.iter().find(|w| w.app_id=="builtin.settings").unwrap();
            let (x,y)=s.system_settings.debug_point(w.width as f32,w.height as f32,&s.settings_context(),|a| matches!(a,A::Audio(C::InputVolume { .. }))).unwrap();
            let (gx,gy)=(w.x as f64+x as f64,(w.y+w.titlebar_h()) as f64+y as f64);
            s.debug_pointer_motion(gx,gy);s.debug_pointer_button(0x110,true);
            for step in 1..=20 { s.debug_pointer_motion(gx+step as f64*2.0,gy); }
            assert!(s.system_settings.audio_sent_volume.is_some_and(|v| v>0.55),"slow drag must enqueue intermediate commands");
            eprintln!("[feather-microphone-check] slow drag commands queued before release: ok");
        })?;
        at(state,4500,|s| {
            assert!(s.shell.cc.audio.current_source().unwrap().volume()>0.51,"slow drag must apply before release");
            assert!(s.system_settings.audio_drag.is_some());
            s.debug_pointer_button(0x110,false);
            assert!(s.system_settings.audio_drag.is_none());
        })?;
        at(state,5100,|s| {
            let mic=s.shell.cc.audio.current_source().unwrap();
            assert!(mic.muted && mic.volume()>0.55 && mic.volume()<0.7);
            eprintln!("[feather-microphone-check] selection/volume/mute/recording-move: ok");
        })?;
        at(state,6400,|s| {
            click(s,|a| matches!(a,A::Audio(C::InputMute { .. })));
        })?;
        at(state,6700,|s| {
            assert!(s.system_settings.error.is_some(),"a new snapshot must not erase a failed command");
            eprintln!("[feather-microphone-check] command error survives snapshot: ok");
            let audio=s.settings_context().audio;
            assert_eq!(audio.default_source,"builtin mic");
            assert!((audio.current_source().unwrap().volume()-0.25).abs()<0.001);
            assert!(audio.current_source().unwrap().unplugged);
            s.apply_system_setting(A::Page(Page::Sound));
            assert!(s.system_settings.error.is_none(),"an explicit action clears the command error");
            eprintln!("[feather-microphone-check] external changes: ok");
        })?;
        at(state,7900,|s| {
            assert!(s.settings_context().audio.sources.is_empty());
            assert!(s.shell.cc.audio.current().is_some(),"playback survives input removal");
            eprintln!("[feather-microphone-check] unplug: ok");
        })?;
        at(state,9100,|s| {
            assert!(s.settings_context().audio.input_error.is_some());
            assert!(s.shell.cc.audio.current().is_some());
            eprintln!("[feather-microphone-check] input error: ok");
        })?;
        at(state,10400,|s| {
            assert_eq!(s.settings_context().audio.current_source().unwrap().name,"usb mic");
            assert!(s.shell.cc.audio.input_error.is_none());
            eprintln!("[feather-microphone-check] reconnect: ok");
            eprintln!("[feather-microphone-check] complete: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_NETWORK_SETTINGS").is_some() {
        use crate::{controls::Control as C, system_settings::{Action as A, Page}};
        if state.backend.is_drm() || !state.debug_input_only {
            return Err("network settings check requires isolated windowed input".into());
        }
        fn click(s: &mut FeatherCompositor, predicate: impl Fn(&A) -> bool) {
            let w = s.windows.iter().find(|w| w.app_id == "builtin.settings").unwrap();
            let (width, height) = (w.width as f32, w.height as f32);
            let (_, y) = s.system_settings.debug_point(width, height, &s.settings_context(), &predicate).unwrap();
            s.system_settings.scroll += (y - height * 0.55).max(0.0);
            let (x, y) = s.system_settings.debug_point(width, height, &s.settings_context(), &predicate).unwrap();
            let w = s.windows.iter().find(|w| w.app_id == "builtin.settings").unwrap();
            let (gx, gy) = (w.x as f64 + x as f64, (w.y + w.titlebar_h()) as f64 + y as f64);
            assert!(y > 12.0 && y < height - 20.0);
            s.debug_pointer_motion(gx, gy);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
        }
        at(state, 800, |s| {
            s.shell.control_center_open = true;
            s.shell.cc.open(crate::control_panel::Page::Wifi);
            s.execute_shell_action(ShellClickAction::NetworkSettings);
            assert!(!s.shell.control_center_open);
            assert_eq!(s.system_settings.page, Page::Network);
            assert!(s.windows.iter().any(|w| w.app_id == "builtin.settings" && w.is_visible()));
            eprintln!("[feather-network-check] Control Center -> Network Settings: ok");
        })?;
        at(state, 1500, |s| {
            let wifi = &s.shell.cc.wifi;
            assert!(wifi.enabled);
            assert_eq!(wifi.connected.as_deref(), Some("Feather 5G"));
            assert_eq!(wifi.saved_wifi.len(), 2);
            assert_eq!(wifi.ethernets.len(), 1);
            assert!(wifi.ethernets[0].active && wifi.ethernets[0].speed == 1000);
            assert_eq!(wifi.vpns.len(), 2);
            assert!(wifi.vpns.iter().any(|v| v.label == "Work WireGuard" && v.active));
            assert!(wifi.vpns.iter().any(|v| v.label == "Corp VPN" && !v.active));
            eprintln!("[feather-network-check] initial snapshot (Wi-Fi, Saved, Ethernet, VPN): ok");
            click(s, |a| matches!(a, A::Network(C::SetAutoconnect { profile, autoconnect: false }) if profile == "/test/saved/home"));
        })?;
        at(state, 2200, |s| {
            let home = s.shell.cc.wifi.saved_wifi.iter().find(|w| w.profile == "/test/saved/home").unwrap();
            assert!(!home.autoconnect);
            click(s, |a| matches!(a, A::Network(C::ForgetConnection(profile)) if profile == "/test/saved/cafe"));
        })?;
        at(state, 2900, |s| {
            assert_eq!(s.shell.cc.wifi.saved_wifi.len(), 1);
            assert_eq!(s.shell.cc.wifi.saved_wifi[0].label, "Feather 5G");
            eprintln!("[feather-network-check] saved Wi-Fi autoconnect + forget: ok");
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::Network(C::Disconnect(dev)) if dev == "/test/eth0"));
        })?;
        at(state, 3600, |s| {
            assert!(!s.shell.cc.wifi.ethernets[0].active);
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::Network(C::ConnectEthernet { device, .. }) if device == "/test/eth0"));
        })?;
        at(state, 4300, |s| {
            assert!(s.shell.cc.wifi.ethernets[0].active);
            eprintln!("[feather-network-check] Ethernet disconnect + connect: ok");
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::Network(C::DisconnectVpn(active)) if active == "/test/active/wg"));
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::Network(C::ConnectVpn(profile)) if profile == "/test/vpn/corp"));
        })?;
        at(state, 5100, |s| {
            let wg = s.shell.cc.wifi.vpns.iter().find(|v| v.profile == "/test/vpn/wg").unwrap();
            let corp = s.shell.cc.wifi.vpns.iter().find(|v| v.profile == "/test/vpn/corp").unwrap();
            assert!(!wg.active && corp.active);
            eprintln!("[feather-network-check] VPN connect + disconnect: ok");
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::WifiPassword(n) if n.label == "Neighbor WPA2"));
            assert!(s.system_settings.wifi_target.is_some());
            // Type "feather123" (f=33, e=18, a=30, t=20, h=35, e=18, r=19, 1=2, 2=3, 3=4) and press Enter (28).
            for key in [33, 18, 30, 20, 35, 18, 19, 2, 3, 4] {
                tap(s, key);
            }
            assert_eq!(s.system_settings.wifi_password.text(), "feather123");
            tap(s, 28);
        })?;
        at(state, 5900, |s| {
            assert!(s.system_settings.wifi_target.is_none());
            assert_eq!(s.shell.cc.wifi.connected.as_deref(), Some("Neighbor WPA2"));
            assert_eq!(s.shell.cc.wifi.saved_wifi.len(), 2);
            eprintln!("[feather-network-check] WPA2 password sheet + connect: ok");
            s.system_settings.scroll = 0.0;
            // Disconnecting corp VPN is configured in the mock to fail once, testing error persistence across a signal snapshot.
            click(s, |a| matches!(a, A::Network(C::DisconnectVpn(active)) if active == "/test/active/corp"));
        })?;
        at(state, 6600, |s| {
            assert!(s.system_settings.error.is_some(), "command error must survive snapshot");
            eprintln!("[feather-network-check] command error survives snapshot: ok");
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::Network(C::RefreshNetwork)));
            assert!(s.system_settings.error.is_none(), "explicit action clears error");
            s.system_settings.scroll = 0.0;
            s.mark_settings_dirty();
            s.needs_redraw = true;
            eprintln!("[feather-network-check] complete: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_BLUETOOTH_SETTINGS").is_some() {
        use crate::{controls::Control as C, system_settings::{Action as A, Page}};
        if state.backend.is_drm() || !state.debug_input_only {
            return Err("bluetooth settings check requires isolated windowed input".into());
        }
        fn click(s: &mut FeatherCompositor, predicate: impl Fn(&A) -> bool) {
            let w = s.windows.iter().find(|w| w.app_id == "builtin.settings").unwrap();
            let (width, height) = (w.width as f32, w.height as f32);
            let (_, y) = s.system_settings.debug_point(width, height, &s.settings_context(), &predicate).unwrap();
            s.system_settings.scroll += (y - height * 0.55).max(0.0);
            let (x, y) = s.system_settings.debug_point(width, height, &s.settings_context(), &predicate).unwrap();
            let w = s.windows.iter().find(|w| w.app_id == "builtin.settings").unwrap();
            let (gx, gy) = (w.x as f64 + x as f64, (w.y + w.titlebar_h()) as f64 + y as f64);
            assert!(y > 12.0 && y < height - 20.0);
            s.debug_pointer_motion(gx, gy);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
        }
        at(state, 800, |s| {
            s.shell.control_center_open = true;
            s.shell.cc.open(crate::control_panel::Page::Bluetooth);
            s.execute_shell_action(ShellClickAction::Control(C::ScanBluetooth));
            assert!(s.shell.cc.page == crate::control_panel::Page::Bluetooth);
            eprintln!("[feather-bluetooth-check] Control Center Bluetooth page + scan: ok");
        })?;
        at(state, 1500, |s| {
            let bt = &s.shell.cc.bluetooth;
            assert!(bt.available && bt.powered && bt.discovering);
            assert_eq!(bt.devices.len(), 3, "unnamed BLE beacon must be filtered out");
            let pods = bt.devices.iter().find(|d| d.name == "AirPods Pro").unwrap();
            assert!(pods.connected && pods.paired && pods.battery == Some(88));
            s.execute_shell_action(ShellClickAction::BluetoothSettings);
            assert!(!s.shell.control_center_open);
            assert_eq!(s.system_settings.page, Page::Bluetooth);
            assert!(s.windows.iter().any(|w| w.app_id == "builtin.settings" && w.is_visible()));
            eprintln!("[feather-bluetooth-check] Control Center -> Bluetooth Settings + snapshot: ok");
        })?;
        at(state, 2200, |s| {
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::Bluetooth(C::ConnectBluetooth(p)) if p == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03"));
        })?;
        at(state, 2900, |s| {
            let mouse = s.shell.cc.bluetooth.devices.iter().find(|d| d.path == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03").unwrap();
            assert!(mouse.paired && mouse.trusted && mouse.connected);
            eprintln!("[feather-bluetooth-check] pair + trust + connect discovered device: ok");
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::Bluetooth(C::DisconnectBluetooth(p)) if p == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01"));
        })?;
        at(state, 3600, |s| {
            let pods = s.shell.cc.bluetooth.devices.iter().find(|d| d.path == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01").unwrap();
            assert!(!pods.connected && pods.paired);
            eprintln!("[feather-bluetooth-check] disconnect device: ok");
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::Bluetooth(C::ForgetBluetooth(p)) if p == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02"));
        })?;
        at(state, 4300, |s| {
            assert_eq!(s.shell.cc.bluetooth.devices.len(), 2);
            assert!(!s.shell.cc.bluetooth.devices.iter().any(|d| d.path == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02"));
            eprintln!("[feather-bluetooth-check] forget device (RemoveDevice): ok");
            s.system_settings.scroll = 0.0;
            // Reconnecting AirPods Pro is configured in the mock to fail once, verifying error persistence across a signal snapshot.
            click(s, |a| matches!(a, A::Bluetooth(C::ConnectBluetooth(p)) if p == "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01"));
        })?;
        at(state, 5000, |s| {
            assert!(s.system_settings.error.is_some(), "command error must survive snapshot");
            eprintln!("[feather-bluetooth-check] command error survives snapshot: ok");
            s.system_settings.scroll = 0.0;
            click(s, |a| matches!(a, A::Bluetooth(C::ScanBluetooth)));
            assert!(s.system_settings.error.is_none(), "explicit action clears error");
        })?;
        at(state, 5700, |s| {
            assert!(!s.shell.cc.bluetooth.discovering);
            s.system_settings.scroll = 0.0;
            s.mark_settings_dirty();
            s.needs_redraw = true;
            eprintln!("[feather-bluetooth-check] stop discovery + complete: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_AUDIO_KEYS").is_some() {
        if state.backend.is_drm() || !state.debug_input_only {return Err("audio key check requires isolated windowed input".into());}
        let level = Rc::new(Cell::new(0.0f32));
        at(state, 1200, |s| {
            let sink=s.shell.cc.audio.current().expect("fake audio ready");
            assert!((sink.volume()-0.5).abs()<0.001);
            tap(s,115);
        })?;
        at(state, 1800, |s| {
            assert!((s.shell.cc.audio.current().unwrap().volume()-0.55).abs()<0.001);
            assert_eq!(s.audio_hud.percent,55);assert!(s.audio_hud.visible);
            tap(s,114);tap(s,114);tap(s,113);tap(s,113);
        })?;
        at(state, 2400, |s| {
            let a=s.shell.cc.audio.current().unwrap();assert!((a.volume()-0.45).abs()<0.001 && !a.muted);
            s.debug_keyboard_key(115,true);
        })?;
        at(state, 3300, |s| {s.debug_keyboard_key(115,false);})?;
        let value=level.clone();
        at(state, 3650, move |s| {
            let a=s.shell.cc.audio.current().unwrap();assert!((0.85..=1.0).contains(&a.volume()));value.set(a.volume());
        })?;
        let value=level.clone();
        at(state, 4100, move |s| {
            assert_eq!(s.shell.cc.audio.current().unwrap().volume(),value.get(),"release stops volume changes");
            s.debug_keyboard_key(113,true); // Mute must never repeat while held.
        })?;
        at(state, 4900, |s| {
            assert!(s.shell.cc.audio.current().unwrap().muted);assert!(s.audio_hud.muted);
            s.debug_keyboard_key(113,false);
            s.debug_keyboard_key(114,true);
        })?;
        at(state, 5450, |s| {s.debug_keyboard_key(114,false);})?;
        at(state, 5900, |s| {
            assert!(!s.shell.cc.audio.current().unwrap().muted,"volume keys unmute");
            for _ in 0..10 {tap(s,115);}
        })?;
        at(state, 6500, |s| {
            assert!((s.shell.cc.audio.current().unwrap().volume()-1.0).abs()<0.001,"clamped at 100%");
            eprintln!("[feather-audio-key-check] steps/burst/balance/mute/hold/release: ok");
        })?;
        // The harness changes the default output at 7.0 s and removes it at 8.3 s.
        at(state, 7600, |s| {tap(s,115);})?;
        at(state, 8100, |s| {
            let a=s.shell.cc.audio.current().unwrap();assert_eq!(a.name,"usb output");assert!((a.volume()-0.35).abs()<0.001);
            eprintln!("[feather-audio-key-check] default output changed: ok");
        })?;
        at(state, 8900, |s| {tap(s,115);})?;
        at(state, 9400, |s| {
            assert!(s.shell.cc.audio.current().is_none());assert!(s.audio_hud.visible && !s.audio_hud.available);
            for key in [68,87,88] {tap(s,key);} // F10/F11/F12 stay normal function keys.
        })?;
        at(state, 11100, |s| {
            assert!(!s.audio_hud.visible,"feedback expires");
            eprintln!("[feather-audio-key-check] disconnected/normal F-keys/expiry: ok");
            eprintln!("[feather-audio-key-check] complete: ok");
        })?;
    }
    if let Ok(mode) = std::env::var("FEATHER_DEBUG_KEYBOARD") {
        use crate::{shortcuts::{Action as K, Chord, CTRL, SUPER}, system_settings::{Action as A, Page}};
        fn search(s: &mut FeatherCompositor) {
            s.debug_keyboard_key(29, true);
            chord(s, 125, 25); // Ctrl+Super+P, independently of the current language
            s.debug_keyboard_key(29, false);
        }
        fn choose(s: &mut FeatherCompositor, action: K) {
            let w = s.windows.iter().find(|w| w.app_id == "builtin.settings").unwrap();
            let (id, width, height) = (w.id, w.width as f32, w.height as f32);
            let predicate = |a: &A| matches!(a, A::EditShortcut(k) if *k == action);
            let (_, y) = s.system_settings.debug_point(width, height, &s.settings_context(), predicate).unwrap();
            s.system_settings.scroll += (y - height * 0.5).max(0.0);
            let (x, y) = s.system_settings.debug_point(width, height, &s.settings_context(), predicate).unwrap();
            assert!(y > 50.0 && y < height - 24.0);
            let w = s.windows.iter().find(|w| w.id == id).unwrap();
            let (gx, gy) = (w.x as f64 + x as f64, (w.y + w.titlebar_h()) as f64 + y as f64);
            s.debug_pointer_motion(gx, gy);
            s.debug_pointer_button(0x110, true);s.debug_pointer_button(0x110, false);
            assert_eq!(s.system_settings.shortcut_capture, Some(action));
        }
        if mode == "restore" {
            at(state, 1000, |s| {
                assert_eq!(s.keybindings.chord(K::Search), Some(Chord {mods:CTRL|SUPER,key:112}));
                assert_eq!(s.keybindings.chord(K::Files), None);
                assert_eq!((s.keyboard_config.repeat_delay,s.keyboard_config.repeat_rate),(400,20));
                search(s);assert!(s.shell.spotlight_open);
                tap(s,1);assert!(!s.shell.spotlight_open);
                eprintln!("[feather-keyboard-check] restored: ok");
            })?;
        } else {
            let count = Rc::new(Cell::new(0usize));
            at(state, 1200, |s| {
                s.apply_system_setting(A::Page(Page::Keyboard));
                s.apply_system_setting(A::RepeatDelay(400));s.apply_system_setting(A::RepeatRate(20));
                choose(s,K::Search);search(s);
                assert!(!s.shell.spotlight_open,"capture must not perform the shortcut");
                assert_eq!(s.keybindings.chord(K::Search), Some(Chord {mods:CTRL|SUPER,key:112}));
                choose(s,K::Files);search(s);
                assert!(s.system_settings.error.is_some());assert!(s.capturing_shortcut());
                tap(s,1);assert!(!s.capturing_shortcut());
                choose(s,K::Files);tap(s,14);assert_eq!(s.keybindings.chord(K::Files),None);
                chord(s,125,57);assert!(!s.shell.spotlight_open,"old chord is unbound");
                s.select_input_source(1);search(s);assert!(s.shell.spotlight_open,"Latin binding in Russian");
                search(s);assert!(!s.shell.spotlight_open);s.select_input_source(0);
                search(s);s.debug_keyboard_key(30,true);
                eprintln!("[feather-keyboard-check] capture/conflict/disable/Russian: ok");
            })?;
            at(state, 1450, |s| {assert_eq!(s.shell.spotlight_query,"a","no repeat before delay");})?;
            let n=count.clone();
            at(state, 2100, move |s| {
                let len=s.shell.spotlight_query.len();assert!((5..=14).contains(&len),"repeat count {len}");
                s.debug_keyboard_key(30,false);n.set(len);
                eprintln!("[feather-keyboard-check] delay/rate: {len} chars, ok");
            })?;
            let n=count.clone();
            at(state, 2400, move |s| {
                assert_eq!(s.shell.spotlight_query.len(),n.get(),"release stops repeat");
                s.debug_keyboard_key(42,true);s.debug_keyboard_key(105,true); // Shift+Left
            })?;
            at(state, 3100, |s| {
                assert!(s.shell.spotlight_input.selected_text().unwrap().len()>=3,"held selection");
                s.debug_keyboard_key(105,false);s.debug_keyboard_key(42,false);
                s.debug_keyboard_key(14,true);
            })?;
            at(state, 3900, |s| {
                s.debug_keyboard_key(14,false);assert!(s.shell.spotlight_query.is_empty(),"held backspace");
                s.debug_keyboard_key(48,true);s.release_all_input();
            })?;
            at(state, 4500, |s| {
                assert_eq!(s.shell.spotlight_query,"b","lost host focus stops repeat");
                s.shell.spotlight_open=false;
                s.apply_system_setting(A::Page(Page::DateTime));s.system_settings.zone_picker=true;
                s.debug_keyboard_key(30,true);
            })?;
            at(state, 5300, |s| {
                assert!(s.system_settings.zone_query.len()>=5,"settings field repeats");s.debug_keyboard_key(30,false);
                s.system_settings.zone_picker=false;
                s.system_settings.time_editing=true;s.system_settings.time_input.clear();
                s.debug_keyboard_key(2,true); // 1
            })?;
            at(state, 6100, |s| {
                assert!(s.system_settings.time_input.len()>=5);s.debug_keyboard_key(2,false);
                s.system_settings.time_editing=false;
                s.shell.launchpad=Some(crate::launchpad::Launchpad::new(&s.app_registry,s.shell.screen_w,s.shell.screen_h));
                s.debug_keyboard_key(30,true);
            })?;
            at(state, 6900, |s| {
                let lp=s.shell.launchpad.as_ref().unwrap();assert!(lp.input.len()>=5,"Launchpad repeat");
                s.debug_keyboard_key(30,false);tap(s,1);
                s.shell.spotlight_input.clear();s.shell.spotlight_query.clear();search(s);
                s.debug_keyboard_key(30,true);
                // A click must immediately cancel the timer, even if the field stays open.
                s.debug_pointer_motion(8.0,200.0);s.debug_pointer_button(0x111,true);s.debug_pointer_button(0x111,false);
            })?;
            at(state, 7500, |s| {
                assert_eq!(s.shell.spotlight_query,"a","click stops repeat");
                s.shell.close_menus();s.shell.spotlight_open=false;
                s.apply_system_setting(A::Page(Page::Keyboard));
                choose(s,K::Search); // show recording state in screenshot
                eprintln!("[feather-keyboard-check] release/selection/delete/focus/fields/click: ok");
            })?;
            at(state, 9000, |s| {
                tap(s,1);assert!(!s.capturing_shortcut());chord(s,125,17);
                eprintln!("[feather-keyboard-check] complete: ok");
            })?;
        }
    }
    if std::env::var_os("FEATHER_DEBUG_MINIMIZE").is_some() {
        // A Tauri-like client (bench/tauri-like.py): its own HTML titlebar buttons must keep
        // working after it minimizes itself and comes back from the Dock.
        fn click(s: &mut FeatherCompositor, dx: f64, dy: f64) {
            let Some(w) = s.windows.iter().find(|w| w.app_id.contains("tauri") && w.mapped) else { return eprintln!("[feather-minimize-check] no window") };
            let (cx, cy) = w.client_origin();
            let p: smithay::utils::Point<f64, smithay::utils::Logical> = (cx as f64 + dx, cy as f64 + dy).into();
            let t = s.start_time.elapsed().as_millis() as u32;
            s.on_pointer_motion(p, t);
            s.on_pointer_button(0x110, smithay::backend::input::ButtonState::Pressed, t + 1);
            s.on_pointer_button(0x110, smithay::backend::input::ButtonState::Released, t + 40);
        }
        at(state, 4000, |s| { click(s, 110.0, 20.0); eprintln!("[feather-minimize-check] ping"); })?;
        at(state, 4600, |s| { click(s, 40.0, 20.0); eprintln!("[feather-minimize-check] minimize"); })?;
        at(state, 5400, |s| {
            let Some(w) = s.windows.iter().find(|w| w.app_id.contains("tauri")) else { return eprintln!("[feather-minimize-check] window gone (hidden)") };
            eprintln!("[feather-minimize-check] minimized={}", w.minimized);
            let (id, key) = (w.id, s.app_key(w));
            match std::env::var("FEATHER_DEBUG_MINIMIZE").as_deref() {
                // Dock icon click and Alt+Tab both go through activate_app
                Ok("dock") => s.activate_app(key),
                // The app restores itself (tray): nothing to do here
                Ok("present") | Ok("hide") => {}
                _ => s.execute_shell_action(crate::shell::ShellClickAction::RestoreMinimizedWindow(id)),
            }
            if let Some(w) = s.windows.iter().find(|w| w.id == id) {
                eprintln!("[feather-minimize-check] restored: minimized={}", w.minimized);
            }
        })?;
        at(state, 6400, |s| {
            for w in s.windows.iter().filter(|w| w.app_id.contains("tauri")) {
                eprintln!("[feather-minimize-check] before ping: id={} mapped={} minimized={} visible={} ssd={} geo={:?}", w.id, w.mapped, w.minimized, w.is_visible(), w.has_titlebar(), (w.x, w.y, w.width, w.height));
            }
            click(s, 110.0, 20.0);
            eprintln!("[feather-minimize-check] ping after restore");
        })?;
        at(state, 7000, |s| { click(s, 180.0, 20.0); eprintln!("[feather-minimize-check] close"); })?;
    }
    if std::env::var_os("FEATHER_DEBUG_LAUNCHER").is_some() {
        // Drag an app out of Launchpad onto the bare desktop: a launcher file appears there
        at(state, 1500, |s| {
            let app = s.app_registry.apps.iter().position(|a| matches!(a.action, crate::apps::AppAction::Exec(_)) && a.id != crate::apps::AppRegistry::PERMANENT).unwrap();
            let entry = s.app_registry.apps[app].clone();
            s.pointer_location = (120.0, 220.0).into();
            s.begin_icon_press(crate::DragSource::Launchpad { app }, crate::shell::AppIcon::of(&entry), entry.name.clone(), crate::shell::ShellClickAction::None);
            for (x, y) in [(135.0, 235.0), (150.0, 260.0)] {
                s.pointer_location = (x, y).into();
                s.update_icon_drag();
            }
            assert!(s.icon_drag.as_ref().unwrap().active);
            s.finish_icon_press();
            eprintln!("[feather-launcher-check] dropped {} on the desktop", entry.name);
        })?;
        at(state, 3200, |s| {
            let item = s.shell.desktop.items.iter().find(|i| i.launcher.is_some()).expect("launcher listed");
            let l = item.launcher.as_ref().unwrap();
            assert!(l.executable && item.name.ends_with(".desktop"));
            eprintln!("[feather-launcher-check] listed «{}» → {}: ok", l.name, crate::desktop::launcher_command(&l.exec));
        })?;
        at(state, 3400, |s| {
            // Over a window the drag becomes a Wayland drag of a launcher file
            let app = s.app_registry.apps.iter().position(|a| a.id == "firefox").unwrap_or(1);
            let entry = s.app_registry.apps[app].clone();
            let w = s.windows.iter().rev().find(|w| w.is_visible()).expect("a window to drag over");
            let over = ((w.x + w.width / 2) as f64, (w.y + 40) as f64);
            s.pointer_location = (over.0 - 20.0, over.1).into();
            s.begin_icon_press(crate::DragSource::Launchpad { app }, crate::shell::AppIcon::of(&entry), entry.name.clone(), crate::shell::ShellClickAction::None);
            s.pointer_location = over.into();
            s.update_icon_drag();
            assert!(s.icon_drag.is_none(), "handed over to the Wayland drag");
            assert!(s.shell.ghost.is_some(), "the icon stays under the pointer");
            let offered = s.shell.desktop.dragged_paths();
            assert!(offered.len() == 1 && offered[0].exists() && offered[0].extension().is_some_and(|e| e == "desktop"));
            // No program took it; back on the bare desktop it becomes a launcher there
            s.desktop_dnd_dropped();
            s.pointer_location = (150.0, 520.0).into();
            s.desktop_dnd_cancelled();
            assert!(s.shell.ghost.is_none());
            eprintln!("[feather-launcher-check] offered {} to programs, dropped back on the desktop", offered[0].display());
        })?;
        at(state, 5200, |s| {
            let n = s.shell.desktop.items.iter().filter(|i| i.launcher.is_some()).count();
            assert_eq!(n, 2, "the second launcher arrived");
            eprintln!("[feather-launcher-check] second launcher listed: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_MISSION").is_some() {
        // Mission Control without host input: spread, drag a window onto «+», pick a desktop
        at(state, 1200, |s| {
            s.open_or_focus_builtin(crate::apps::BuiltinWindowKind::SystemSettings);
        })?;
        at(state, 2200, |s| {
            s.open_mission();
            let m = s.mission.as_ref().unwrap();
            let shown = s.windows.iter().filter(|w| w.is_visible()).count();
            assert_eq!(m.targets.len(), shown, "every window of the desktop is in the overview");
            assert!(s.surface_under_pointer(10.0, 300.0).is_none(), "clients get no pointer");
            eprintln!("[feather-mission-check] opened with {shown} windows");
        })?;
        at(state, 3000, |s| {
            let (id, r) = s.mission.as_ref().unwrap().targets[0];
            let add = crate::mission::strip(s.spaces.count, s.shell.screen_w as f32).into_iter().find(|(h, _)| *h == crate::mission::Hit::Add).unwrap().1;
            let (x, y) = ((r.x + r.w * 0.5) as f64, (r.y + r.h * 0.5) as f64);
            s.mission_button(true, x, y);
            s.mission_motion(x + 30.0, y - 30.0);
            let (ax, ay) = ((add.x + add.w * 0.5) as f64, (add.y + add.h * 0.5) as f64);
            s.mission_motion(ax, ay);
            s.mission_button(false, ax, ay);
            let w = s.windows.iter().find(|w| w.id == id).unwrap();
            assert_eq!((w.space, s.spaces.count), (1, 2), "dropped on «+»: a new desktop");
            assert!(s.mission.as_ref().unwrap().targets.iter().all(|(t, _)| *t != id), "left the overview");
            assert!(s.mission_open());
            eprintln!("[feather-mission-check] dragged a window to a new desktop");
        })?;
        at(state, 3600, |s| {
            // × of the new desktop: its window comes back to the first one
            let tile = crate::mission::strip(s.spaces.count, s.shell.screen_w as f32)[1].1;
            s.mission_motion((tile.x + tile.w * 0.5) as f64, (tile.y + tile.h * 0.5) as f64);
            let (x, y) = ((tile.x + 4.0) as f64, (tile.y + 4.0) as f64);
            s.mission_motion(x, y);
            s.mission_button(true, x, y);
            s.mission_button(false, x, y);
            assert_eq!((s.spaces.current, s.spaces.count), (0, 1), "desktop removed");
            assert!(s.windows.iter().filter(|w| w.mapped && !w.minimized).all(|w| w.space == 0 && w.is_visible()));
            assert_eq!(s.mission.as_ref().unwrap().targets.len(), s.windows.iter().filter(|w| w.is_visible()).count(), "its window is back in the overview");
            eprintln!("[feather-mission-check] removed a desktop with ×");
        })?;
        at(state, 4000, |s| {
            let dock = (s.shell.screen_w as f64 * 0.5, s.shell.screen_h as f64 - 30.0);
            assert!(s.shell.dock_contains(dock.0, dock.1));
            s.close_mission(None);
            assert!(!s.mission_open());
        })?;
        at(state, 4800, |s| {
            assert!(s.mission.is_none(), "closed after the spring");
            eprintln!("[feather-mission-check] closed: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_SPACES").is_some() {
        // Desktops without host input: carry a window right, come back, let the Dock
        // bring its desktop forward, carry it home and watch the empty desktop go away.
        at(state, 1200, |s| {
            s.open_or_focus_builtin(crate::apps::BuiltinWindowKind::SystemSettings);
        })?;
        at(state, 1800, |s| {
            let id = s.windows.iter().rev().find(|w| w.is_visible()).map(|w| w.id).unwrap();
            s.space_step(1, true);
            assert_eq!((s.spaces.current, s.spaces.count), (1, 2));
            let w = s.windows.iter().find(|w| w.id == id).unwrap();
            assert!(w.space == 1 && w.is_visible(), "carried window is in front");
            assert!(s.windows.iter().filter(|w| w.id != id && w.mapped).all(|w| !w.is_visible()), "others hidden");
            eprintln!("[feather-spaces-check] carried window to a new desktop");
        })?;
        at(state, 2600, |s| {
            s.space_step(-1, false);
            assert_eq!((s.spaces.current, s.spaces.count), (0, 2), "a desktop with a window stays");
            let w = s.windows.iter().find(|w| w.space == 1).unwrap();
            assert!(!w.is_visible() && s.switcher_items().iter().any(|i| i.key == s.app_key(w)), "hidden but in Alt+Tab");
            let key = s.app_key(w);
            s.activate_app(key);
            assert_eq!(s.spaces.current, 1, "activating its app goes to its desktop");
            eprintln!("[feather-spaces-check] Alt+Tab/Dock bring the window's desktop forward");
        })?;
        at(state, 3400, |s| {
            s.space_step(-1, true);
            assert_eq!((s.spaces.current, s.spaces.count), (0, 1), "the emptied desktop went away");
            assert!(s.windows.iter().filter(|w| w.mapped && !w.minimized).all(|w| w.is_visible()));
            assert!(s.space_step_noop_left(), "nothing left of the first desktop");
            eprintln!("[feather-spaces-check] carried home, empty desktop removed: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_EXTERNAL_LOCK_CHECK").is_some(){
        if state.backend.is_drm() || !state.debug_input_only {return Err("external lock check requires isolated windowed input".into());}
        at(state,2500,|st|{
            assert!(st.lock.external());
            st.debug_pointer_motion(100.0,100.0);st.debug_pointer_button(0x110,true);
            assert!(st.pressed_buttons.contains(&0x110));
            st.debug_pointer_motion(130.0,140.0);st.debug_pointer_button(0x110,false);
            assert!(st.pressed_buttons.is_empty());
            eprintln!("[feather-lock-check] external client receives motion while button held; releases tracked");
        })?;
    }

    if std::env::var_os("FEATHER_DEBUG_LOCK_CHECK").is_some(){
        if state.backend.is_drm() || !state.debug_input_only {return Err("lock check requires isolated windowed input".into());}
        at(state,2000,|st|{
            assert!(!st.lock.active);assert!(!st.windows.is_empty());
            chord(st,125,38); // Super+L through the ordinary keyboard filter
            assert!(st.lock.active);assert!(st.keyboard.current_focus().is_none());
            let layout=st.active_layout_index;chord(st,56,42);assert_ne!(st.active_layout_index,layout);
            chord(st,56,42);assert_eq!(st.active_layout_index,layout);
            assert!(st.surface_under_pointer(200.0,200.0).is_none());
            tap(st,1);chord(st,56,15);tap(st,99);chord(st,125,18);
            assert!(st.lock.active && st.shell.switcher.is_none());
            st.debug_pointer_motion(100.0,100.0);st.debug_pointer_button(0x110,true);st.debug_pointer_button(0x110,false);
            assert!(st.active_drag.is_none());assert!(st.keyboard.current_focus().is_none());
            eprintln!("[feather-lock-check] Super+L captures keyboard/pointer, Escape/Alt+Tab/Print/Super+E cannot escape");
        })?;
        at(state,3000,|st|st.debug_power_check())?;
    }

    if std::env::var_os("FEATHER_DEBUG_DOCK_RESTORE").is_some() {
        assert!(state.dock_locked, "saved Dock lock was not restored");
        let files = state.app_registry.index_of(crate::apps::AppRegistry::PERMANENT).unwrap();
        assert_eq!(state.pinned_indices().last(), Some(&files), "saved Files position was not restored");
        at(state, 1000, move |s| {
            let target = DockTarget::App(files);
            let (x, y, size) = s.shell.dock_slot_global(target).unwrap();
            s.open_dock_menu_for(target, R::new(x, y, size, size));
            eprintln!("[feather-dock-restore-check] saved lock and Files position restored");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_DOCK_PINS").is_some() {
        assert!(std::env::var("XDG_CONFIG_HOME").unwrap_or_default().starts_with("/tmp/"));
        fn point(s: &mut FeatherCompositor, target: DockTarget) -> (f64, f64) {
            let items = s.build_dock_items();
            s.shell.set_dock_items(items);
            s.shell.ensure_dock_rendered(&mut s.fonts);
            let (x, y, size) = s.shell.dock_slot_global(target).expect("Dock icon missing");
            ((x + size * 0.5) as f64, (y + size * 0.5) as f64)
        }
        fn menu(s: &mut FeatherCompositor, target: DockTarget, action: DockMenuAction) {
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            // Hover can resize the icon before the press in a real frame, too.
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            s.debug_pointer_button(0x111, true);
            s.debug_pointer_button(0x111, false);
            let m = s.shell.dock_menu.as_ref().expect("right-click did not open menu");
            let row = m.items.iter().position(|(_, a)| *a == action).expect("menu action missing");
            let x = (m.rect.x + m.rect.w * 0.5) as f64;
            let y = (m.rect.y + 5.0 + (row as f32 + 0.5) * ((m.rect.h - 10.0) / m.items.len() as f32)) as f64;
            s.debug_pointer_motion(x, y);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
            assert!(s.shell.dock_menu.is_none());
        }
        fn drag(s: &mut FeatherCompositor, target: DockTarget, to: (f64, f64)) {
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_motion(to.0, to.1);
            if s.dock_locked {
                assert!(!s.icon_drag.as_ref().unwrap().active);
                assert!(s.shell.dock_gap.is_none());
                assert!(s.shell.ghost.is_none());
            }
            s.debug_pointer_button(0x110, false);
        }
        let app = state.app_registry.index_of("konsole").unwrap();
        let target = DockTarget::App(app);
        state.unpin_app("konsole");
        at(state, 600, move |s| s.launch_app_instance(app))?;
        at(state, 1000, move |s| s.launch_app_instance(app))?;
        at(state, 2000, move |s| {
            assert!(!s.is_pinned(app), "launch pinned an app");
            assert_eq!(s.windows.iter().filter(|w| w.mapped && s.app_key(w) == crate::shell::AppKey::App(app)).count(), 2);
            let (x, y) = point(s, target);
            // A small move within the running section must not silently pin the app.
            drag(s, target, (x + 8.0, y));
            assert!(!s.is_pinned(app), "motion within running apps pinned the app");
            s.execute_shell_action(ShellClickAction::CloseFocused);
        })?;
        at(state, 2800, move |s| {
            assert_eq!(s.windows.iter().filter(|w| w.mapped && s.app_key(w) == crate::shell::AppKey::App(app)).count(), 1);
            point(s, target);
            s.execute_shell_action(ShellClickAction::MinimizeFocused);
            assert!(s.build_dock_items().iter().any(|i| i.target == target));
            menu(s, target, DockMenuAction::Quit);
        })?;
        at(state, 3600, move |s| {
            assert!(!s.build_dock_items().iter().any(|i| i.target == target), "unpinned app remained after its last window closed");
            eprintln!("[feather-dock-pins-check] two windows, incidental motion, minimize, last close passed");
            s.launch_app_instance(app);
        })?;
        at(state, 4500, move |s| {
            menu(s, target, DockMenuAction::KeepInDock);
            assert!(s.is_pinned(app));
            menu(s, target, DockMenuAction::Quit);
        })?;
        at(state, 5300, move |s| {
            assert!(!s.windows.iter().any(|w| w.mapped && s.app_key(w) == crate::shell::AppKey::App(app)));
            point(s, target);
            menu(s, target, DockMenuAction::RemoveFromDock);
            assert!(!s.build_dock_items().iter().any(|i| i.target == target));
            s.launch_app_instance(app);
            eprintln!("[feather-dock-pins-check] pin survives last close, unpin removes closed app passed");
        })?;
        at(state, 6200, move |s| {
            menu(s, target, DockMenuAction::ToggleDockLock);
            assert!(s.dock_locked);
            let before = s.dock_pins.clone();
            drag(s, target, (20.0, 90.0));
            assert_eq!(s.dock_pins, before);
            menu(s, target, DockMenuAction::KeepInDock);
            assert!(s.is_pinned(app), "manual pin should work while dragging is locked");
            let before = s.dock_pins.clone();
            drag(s, target, (20.0, 90.0));
            assert_eq!(s.dock_pins, before, "locked drag removed pin");
            let first = DockTarget::App(s.app_registry.index_of(crate::apps::AppRegistry::PERMANENT).unwrap());
            let to = point(s, first);
            drag(s, target, to);
            assert_eq!(s.dock_pins, before, "locked drag reordered pins");
            menu(s, target, DockMenuAction::RemoveFromDock);
            assert!(!s.is_pinned(app));
            s.execute_shell_action(ShellClickAction::DockPress(target));
            assert!(s.windows.iter().filter(|w| s.app_key(w) == crate::shell::AppKey::App(app)).all(|w| w.minimized));
            s.execute_shell_action(ShellClickAction::DockPress(target));
            assert!(s.windows.iter().filter(|w| s.app_key(w) == crate::shell::AppKey::App(app)).all(|w| !w.minimized));
            s.toggle_launchpad();
        })?;
        at(state, 7100, move |s| {
            let (app, cell) = s.shell.launchpad.as_ref().unwrap().page_cells().into_iter()
                .find(|(i, _)| !s.is_pinned(*i)).unwrap();
            let before = s.dock_pins.clone();
            s.debug_pointer_motion((cell.x + cell.w * 0.5) as f64, (cell.y + 20.0) as f64);
            s.debug_pointer_button(0x110, true);
            assert!(matches!(s.icon_drag.as_ref().unwrap().source, crate::DragSource::Launchpad { app: i } if i == app));
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            assert!(s.shell.ghost.is_none());
            assert!(s.shell.dock_gap.is_none());
            s.debug_pointer_button(0x110, false);
            assert_eq!(s.dock_pins, before, "locked Launchpad drop pinned an app");
            assert!(s.shell.launchpad.is_some(), "blocked drag turned into a launch");
            s.toggle_launchpad();
            menu(s, target, DockMenuAction::ToggleDockLock);
            assert!(!s.dock_locked);
            eprintln!("[feather-dock-pins-check] locked Dock out/reorder, Launchpad in, clicks and manual pins passed");
        })?;
        at(state, 8000, move |s| {
            menu(s, target, DockMenuAction::KeepInDock);
            drag(s, target, (20.0, 90.0));
            assert!(!s.is_pinned(app), "unlock did not restore drag out");
            s.toggle_launchpad();
        })?;
        at(state, 8800, move |s| {
            let (app, cell) = s.shell.launchpad.as_ref().unwrap().page_cells().into_iter()
                .find(|(i, _)| !s.is_pinned(*i)).unwrap();
            s.debug_pointer_motion((cell.x + cell.w * 0.5) as f64, (cell.y + 20.0) as f64);
            s.debug_pointer_button(0x110, true);
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            assert!(s.shell.ghost.is_some());
            s.debug_pointer_button(0x110, false);
            assert!(s.is_pinned(app), "unlock did not restore Launchpad drop");
            menu(s, target, DockMenuAction::ToggleDockLock);
            assert!(s.dock_locked);
            eprintln!("[feather-dock-pins-check] unlock restores drag in/out; final lock saved");
        })?;
        at(state, 9500, move |s| {
            let (x, y) = point(s, DockTarget::Trash);
            s.debug_pointer_motion(x, y);
            s.debug_pointer_button(0x111, true);
            s.debug_pointer_button(0x111, false);
            let m = s.shell.dock_menu.as_ref().unwrap();
            assert!(m.items.iter().any(|(label, a)| *a == DockMenuAction::ToggleDockLock && label.starts_with('✓')));
            eprintln!("[feather-dock-pins-check] checked lock in Trash menu passed");
        })?;
        at(state, 11000, |s| {
            s.shell.close_menus();
            let p = (0..s.shell.screen_w).flat_map(|x| (s.shell.screen_h - 100..s.shell.screen_h).map(move |y| (x, y)))
                .find(|&(x, y)| s.shell.dock_contains(x as f64, y as f64) && s.shell.dock_target_at(x as f64, y as f64).is_none())
                .expect("Dock background point");
            s.debug_pointer_motion(p.0 as f64, p.1 as f64);
            s.debug_pointer_button(0x111, true);
            s.debug_pointer_button(0x111, false);
            let m = s.shell.dock_menu.as_ref().expect("right-click Dock background");
            assert_eq!(m.items.len(), 1);
            assert_eq!(m.items[0].1, DockMenuAction::ToggleDockLock);
            s.shell.close_menus();
            s.debug_pointer_motion(20.0, 90.0);
        })?;
        at(state, 12000, move |s| {
            let files = s.app_registry.index_of(crate::apps::AppRegistry::PERMANENT).unwrap();
            let target = DockTarget::App(files);
            menu(s, target, DockMenuAction::ToggleDockLock);
            assert!(!s.dock_locked);
            let to = point(s, DockTarget::App(app));
            drag(s, target, (to.0 + 25.0, to.1));
            assert_eq!(s.pinned_indices().last(), Some(&files), "Files forced back to first position");
            let before = s.dock_pins.clone();
            drag(s, target, (20.0, 90.0));
            assert_eq!(s.dock_pins, before, "Files must remain pinned when dragged out");
            menu(s, target, DockMenuAction::ToggleDockLock);
            let to = point(s, DockTarget::App(s.pinned_indices()[0]));
            drag(s, target, to);
            assert_eq!(s.dock_pins, before, "locked Files moved");
            assert_eq!(s.pinned_indices().last(), Some(&files));
            s.debug_pointer_motion(20.0, 90.0);
            eprintln!("[feather-dock-pins-check] Files keeps chosen position, remains pinned, obeys lock");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_TRASH").is_some() {
        // Empties a trash for real: only ever a throwaway XDG_DATA_HOME under /tmp.
        let data = std::env::var("XDG_DATA_HOME").unwrap_or_default();
        assert!(data.starts_with("/tmp/"), "FEATHER_DEBUG_TRASH needs XDG_DATA_HOME under /tmp");
        at(state, 1500, |s| {
            assert!(s.trash_full, "Dock trash should be full");
            s.confirm_empty_trash();
            assert!(s.keyboard.current_focus().is_none());
        })?;
        at(state, 2600, |s| {
            s.debug_keyboard_key(28, true);
            s.debug_keyboard_key(28, false);
        })?;
        at(state, 3600, |s| {
            assert!(!s.auth.is_open());
            assert!(!s.trash_full, "Dock trash should be empty after confirming");
            assert!(feather_ui::fileops::trash_is_empty());
            eprintln!("[feather-trash-check] full icon, confirmation, Enter empties, icon empty");
        })?;
    }
    if let Ok(root) = std::env::var("FEATHER_DEBUG_POLKIT") {
        let socket = std::path::Path::new(&root).join("helper.socket");
        polkit_fake_helper(&socket)?;
        std::env::set_var("FEATHER_POLKIT_HELPER_SOCKET", &socket);
        let agent = crate::polkit_agent::Agent::detached(&state.loop_handle)?;
        state.polkit = Some(agent.clone());
        let me = unsafe { libc::getuid() };
        at(state, 800, move |_| {
            let user = |uid: u32, name: &str, real: &str| crate::polkit_agent::User { uid, name: name.into(), real_name: real.into() };
            agent.inject(crate::polkit_agent::Request {
                cookie: "debug-1".into(),
                action_id: "org.freedesktop.policykit.exec".into(),
                message: "Для запуска «/usr/bin/gparted» от имени суперпользователя требуется аутентификация.".into(),
                details: vec![
                    ("command_line".into(), "/usr/bin/gparted".into()),
                    ("program".into(), "/usr/bin/gparted".into()),
                ],
                users: vec![user(me, "zumka", "Зумка"), user(me + 1, "admin", "")],
                default_user: 0,
            });
        })?;
        // "wrong" + Enter, then "feather" + Enter (evdev codes, US layout).
        for (i, key) in [17u32, 19, 24, 49, 34, 28].into_iter().enumerate() {
            at(state, 1500 + i as u64 * 60, move |s| {
                s.debug_keyboard_key(key, true);
                s.debug_keyboard_key(key, false);
            })?;
        }
        at(state, 3200, |s| {
            assert!(s.auth.is_open(), "dialog closed after a wrong password");
            assert!(s.keyboard.current_focus().is_none(), "a client has keyboard focus under the dialog");
        })?;
        for (i, key) in [33u32, 18, 30, 20, 35, 18, 19, 28].into_iter().enumerate() {
            at(state, 3600 + i as u64 * 60, move |s| {
                s.debug_keyboard_key(key, true);
                s.debug_keyboard_key(key, false);
            })?;
        }
        at(state, 5000, |s| {
            assert!(!s.auth.is_open(), "dialog still open after the right password");
            eprintln!("[feather-polkit-check] wrong password shakes, right password closes, keyboard captured");
        })?;
    }
    if let Ok(root) = std::env::var("FEATHER_DEBUG_DEFAULTS_ROOT") {
        use crate::system_settings::{Action as A, Page};
        fn click(s: &mut FeatherCompositor, p: impl Fn(&A) -> bool) {
            let w = s
                .windows
                .iter()
                .find(|w| w.app_id == "builtin.settings")
                .unwrap();
            let (x, y) = s
                .system_settings
                .debug_point(w.width as f32, w.height as f32, &s.settings_context(), p)
                .unwrap();
            let point = (
                w.x as f64 + x as f64,
                w.y as f64 + w.titlebar_h() as f64 + y as f64,
            );
            s.debug_pointer_motion(point.0, point.1);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
        }
        at(state, 900, |s| {
            s.apply_system_setting(A::Page(Page::Defaults))
        })?;
        at(state, 1700, |s| {
            click(s, |a| matches!(a, A::AppPicker(true)))
        })?;
        at(state, 2300, |s| {
            click(
                s,
                |a| matches!(a,A::DefaultApp{browser:true,id} if id=="feather-check-browser.desktop"),
            )
        })?;
        at(state, 3100, |s| {
            assert_eq!(
                s.system_preferences.browser.as_deref(),
                Some("feather-check-browser.desktop")
            );
            click(s, |a| matches!(a, A::AppPicker(false)));
        })?;
        at(state, 4000, |s| {
            click(
                s,
                |a| matches!(a,A::DefaultApp{browser:false,id} if id=="feather-check-terminal.desktop"),
            )
        })?;
        at(state, 4800, |s| {
            assert_eq!(
                s.system_preferences.terminal,
                "feather-check-terminal.desktop"
            );
            s.debug_keyboard_key(125, true);
            s.debug_keyboard_key(20, true);
            s.debug_keyboard_key(20, false);
            s.debug_keyboard_key(125, false);
        })?;
        at(state, 6300, |s| {
            assert!(s
                .windows
                .iter()
                .any(|w| w.app_id == "feather-terminal" && w.mapped));
        })?;
        at(state, 7000, move |s| {
            let exe = std::env::current_exe().unwrap();
            let dir = std::path::Path::new(&root).join("working-dir");
            let command = format!(
                "\"{}\" --open-terminal \"{}\"",
                exe.display(),
                dir.display()
            );
            s.execute_shell_action(ShellClickAction::LaunchApp(
                crate::apps::AppAction::Exec(command),
                "Terminal helper check".into(),
            ));
        })?;
        at(state, 9500, |s| {
            assert_eq!(
                s.windows
                    .iter()
                    .filter(|w| w.app_id == "feather-terminal" && w.mapped)
                    .count(),
                2
            );
            s.apply_system_setting(A::Page(
                if std::env::var("FEATHER_DEBUG_DEFAULTS_PAGE").as_deref() == Ok("date") {
                    Page::DateTime
                } else {
                    Page::Defaults
                },
            ));
        })?;
        at(state, 10800, |s| {
            assert_eq!(
                s.system_preferences.terminal,
                "feather-check-terminal.desktop"
            );
            assert_eq!(
                s.system_preferences.browser.as_deref(),
                Some("feather-check-browser.desktop")
            );
            eprintln!("[feather-defaults-check] browser associations, terminal hotkey and folder helper passed");
        })?;
    }
    if let Ok(file) = std::env::var("FEATHER_DEBUG_WALLPAPER_FILE") {
        use crate::system_settings::{Action as A, Page};
        at(state, 900, |s| {
            s.apply_system_setting(A::Page(Page::Desktop))
        })?;
        for (ms, index) in [(1700, 0), (3100, 1), (4500, 2)] {
            at(state, ms, move |s| {
                s.apply_system_setting(A::WallpaperImage(Some(
                    crate::wallpaper::ImageSource::Bundled(index),
                )));
            })?;
        }
        at(state, 6000, |s| {
            assert_eq!(
                s.shell.applied_wallpaper_image,
                Some(crate::wallpaper::ImageSource::Bundled(2))
            );
            let w = s
                .windows
                .iter()
                .find(|w| w.app_id == "builtin.settings")
                .unwrap();
            let (x, y) = s
                .system_settings
                .debug_point(
                    w.width as f32,
                    w.height as f32,
                    &s.settings_context(),
                    |a| matches!(a, A::ChooseWallpaper),
                )
                .unwrap();
            let point = (
                w.x as f64 + x as f64,
                w.y as f64 + w.titlebar_h() as f64 + y as f64,
            );
            s.debug_pointer_motion(point.0, point.1);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
        })?;
        let file_check = file.clone();
        at(state, 8000, move |s| {
            assert!(!s.system_settings.wallpaper_busy);
            assert_eq!(
                s.shell.applied_wallpaper_image,
                Some(crate::wallpaper::ImageSource::File(
                    file_check.clone().into()
                ))
            );
            s.set_wallpaper_image(Some(crate::wallpaper::ImageSource::File(
                "/missing-feather-wallpaper.png".into(),
            )));
        })?;
        at(state, 10500, move |s| {
            assert_eq!(
                s.shell.applied_wallpaper_image,
                Some(crate::wallpaper::ImageSource::File(file.clone().into()))
            );
            assert_eq!(s.shell.wallpaper_image, s.shell.applied_wallpaper_image);
            assert!(s.system_settings.error.is_some());
            assert!(s.shell.wallpaper_texture.is_some());
            eprintln!("[feather-wallpaper-check] gallery, picker, custom image, failed file keeps wallpaper passed");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_SETTINGS_RESTORE").is_some() {
        at(state, 1400, |s| {
            assert_eq!(s.keyboard_config.layouts, ["us", "de"]);
            assert_eq!(
                s.keyboard_config.shortcut,
                crate::settings::Shortcut::CtrlSpace
            );
            assert_eq!(s.scale_factor, 2.0);
            assert!(s.animations.enabled());
            s.debug_keyboard_key(29, true);
            s.debug_keyboard_key(57, true);
            s.debug_keyboard_key(57, false);
            s.debug_keyboard_key(29, false);
            assert_eq!(s.active_layout_name, "DE");
            s.apply_system_setting(crate::system_settings::Action::Page(
                crate::system_settings::Page::Keyboard,
            ));
            eprintln!(
                "[feather-settings-check] restored preferences and XKB shortcut after restart"
            );
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_SETTINGS").is_some() {
        use crate::system_settings::{Action as A, Page};
        fn click(state: &mut FeatherCompositor, predicate: impl Fn(&A) -> bool) {
            let w = state
                .windows
                .iter()
                .find(|w| w.app_id == "builtin.settings")
                .unwrap();
            let (x, y) = state
                .system_settings
                .debug_point(
                    w.width as f32,
                    w.height as f32,
                    &state.settings_context(),
                    predicate,
                )
                .expect("settings control");
            let point = (
                w.x as f64 + x as f64,
                (w.y + w.titlebar_h()) as f64 + y as f64,
            );
            state.debug_pointer_motion(point.0, point.1);
            state.debug_pointer_button(0x110, true);
            state.debug_pointer_button(0x110, false);
        }
        at(state, 900, |s| {
            s.apply_system_setting(A::Page(Page::Keyboard))
        })?;
        at(state, 1300, |s| {
            click(s, |a| matches!(a, A::AddPicker(true)))
        })?;
        at(state, 1700, |s| {
            click(s, |a| matches!(a,A::AddSource(id)if id=="de"))
        })?;
        at(state, 2200, |s| {
            assert_eq!(s.keyboard_config.layouts, ["us", "ru", "de"]);
            assert!(!s.system_settings.keyboard_busy);
            click(s, |a| matches!(a, A::AddPicker(false)));
        })?;
        at(state, 2500, |s| {
            click(s, |a| matches!(a, A::SelectSource(2)));
            assert_eq!(s.active_layout_name, "DE");
        })?;
        at(state, 2800, |s| {
            let x = (1..s.shell.screen_w)
                .find(|x| s.shell.debug_input_hit(*x as f64))
                .unwrap();
            s.debug_pointer_motion(x as f64 + 2.0, 12.0);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
            assert_eq!(s.shell.open_menu, Some(crate::shell::TopMenu::Input));
        })?;
        at(state, 3300, |s| {
            s.debug_keyboard_key(108, true);
            s.debug_keyboard_key(108, false);
            s.debug_keyboard_key(28, true);
            s.debug_keyboard_key(28, false);
            assert_eq!(s.active_layout_name, "EN");
        })?;
        at(state, 3500, |s| {
            click(s, |a| matches!(a, A::SelectSource(1)))
        })?;
        at(state, 3700, |s| {
            click(s, |a| matches!(a, A::RemoveSource(1)))
        })?;
        at(state, 4200, |s| {
            assert_eq!(s.keyboard_config.layouts, ["us", "de"]);
            click(s, |a| {
                matches!(a, A::Shortcut(crate::settings::Shortcut::CtrlSpace))
            });
        })?;
        at(state, 4800, |s| {
            assert_eq!(
                s.keyboard_config.shortcut,
                crate::settings::Shortcut::CtrlSpace
            );
            s.debug_keyboard_key(29, true);
            s.debug_keyboard_key(57, true);
            s.debug_keyboard_key(57, false);
            s.debug_keyboard_key(29, false);
            assert_eq!(
                s.active_layout_name, "DE",
                "configured Ctrl+Space did not switch XKB"
            );
            s.apply_system_setting(A::Page(Page::Appearance));
            click(s, |a| matches!(a, A::Animations));
            assert!(!s.animations.enabled());
        })?;
        at(state, 5300, |s| {
            s.open_or_focus_builtin(crate::apps::BuiltinWindowKind::QuickNotes);
            let w = s.windows.last().unwrap();
            assert!(s.animations.window(w).identity());
            assert!(!s.animations.active());
        })?;
        at(state, 5800, |s| {
            s.windows.retain(|w| w.app_id != "builtin.notes");
            s.apply_system_setting(A::Page(Page::Appearance));
            click(s, |a| matches!(a, A::Animations));
            assert!(s.animations.enabled());
        })?;
        at(state, 6300, |s| {
            s.apply_system_setting(A::Page(Page::Desktop));
            click(s, |a| matches!(a, A::DockZoom));
            assert!(!s.shell.dock_zoom_enabled);
        })?;
        at(state, 6800, |s| {
            click(s, |a| matches!(a, A::DockZoom));
            assert!(s.shell.dock_zoom_enabled);
        })?;
        at(state, 7300, |s| {
            s.apply_system_setting(A::Page(Page::Displays));
            click(s, |a| matches!(a,A::Scale(v)if *v==1.25));
            assert_eq!(s.scale_factor, 1.25);
        })?;
        at(state, 7900, |s| {
            click(s, |a| matches!(a,A::Scale(v)if *v==2.0));
            assert_eq!(s.scale_factor, 2.0);
        })?;
        at(state, 8500, |s| {
            let page = match std::env::var("FEATHER_DEBUG_SETTINGS_PAGE").as_deref() {
                Ok("keyboard") | Ok("input") => Page::Keyboard,
                Ok("displays") => Page::Displays,
                Ok("desktop") => Page::Desktop,
                Ok("defaults") => Page::Defaults,
                Ok("date") => Page::DateTime,
                Ok("power") => Page::Power,
                Ok("notifications") => Page::Notifications,
                _ => Page::Appearance,
            };
            s.apply_system_setting(A::Page(page));
            if std::env::var("FEATHER_DEBUG_SETTINGS_PAGE").as_deref() == Ok("input") {
                let x = (1..s.shell.screen_w)
                    .find(|x| s.shell.debug_input_hit(*x as f64))
                    .unwrap();
                s.debug_pointer_motion(x as f64 + 2.0, 12.0);
                s.debug_pointer_button(0x110, true);
                s.debug_pointer_button(0x110, false);
            } else {
                s.debug_pointer_motion(20.0, 60.0);
            }
        })?;
        at(state, 9400, |s| {
            assert_eq!(s.keyboard_config.layouts, ["us", "de"]);
            assert_eq!(s.active_layout_name, "DE");
            assert!(s.animations.enabled());
            assert!(!s.system_settings.keyboard_busy);
            eprintln!("[feather-settings-check] XKB add/remove/menu/Ctrl+Space, animations, Dock and scale passed");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_TRAY_CHECK").is_some() {
        at(state, 2500, |state| {
            assert_eq!(
                state.shell.tray_items.len(),
                2,
                "Qt tray items did not register"
            );
            let x = (1..state.shell.screen_w)
                .find(|x| state.shell.tray_hit(*x as f64, 12.0).is_some())
                .unwrap();
            state.debug_pointer_motion(x as f64 + 2.0, 12.0);
            state.debug_pointer_button(0x111, true);
            state.debug_pointer_button(0x111, false);
        })?;
        at(state, 3600, |state| {
            assert!(
                !state.shell.tray_open_ids().is_empty(),
                "tray menu did not arrive"
            );
            state.shell.client_menu_key(1);
            let action = state
                .shell
                .client_menu_key(0)
                .expect("tray menu first action");
            state.execute_shell_action(action);
        })?;
        at(state, 4600, |state| {
            let x = (1..state.shell.screen_w)
                .find(|x| state.shell.tray_hit(*x as f64, 12.0).is_some())
                .unwrap();
            state.debug_pointer_motion(x as f64 + 2.0, 12.0);
            state.debug_pointer_button(0x111, true);
            state.debug_pointer_button(0x111, false);
        })?;
        at(state, 6500, |state| {
            state.shell.close_menus();
            state.needs_redraw = true;
        })?;
        at(state, 8700, |state| {
            assert_eq!(
                state.shell.tray_items.len(),
                1,
                "hidden Qt icon did not disappear"
            );
        })?;
        at(state, 9700, |state| {
            assert_eq!(state.shell.tray_items.len(), 2, "Qt icon did not return");
            eprintln!(
                "[feather-tray-check] Qt icons, native menu, action, removal and return passed"
            );
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_LAUNCHPAD_CLICK").is_some() {
        at(state, 1500, |state| {
            state.toggle_launchpad();
            if let Some(lp) = state.shell.launchpad.as_mut() {
                lp.set_query(&state.app_registry, "Терминал".into());
            }
        })?;
        at(state, 2300, |state| {
            let (_, r) = state
                .shell
                .launchpad
                .as_ref()
                .unwrap()
                .page_cells()
                .into_iter()
                .next()
                .unwrap();
            state.debug_pointer_motion((r.x + r.w * 0.5) as f64, (r.y + r.h * 0.4) as f64);
            state.debug_pointer_button(0x110, true);
            assert!(state.shell.launchpad.is_some());
            state.debug_pointer_button(0x110, false);
            assert!(
                state.shell.launchpad.is_none(),
                "Launchpad did not close on app launch"
            );
        })?;
        at(state, 3400, |state| {
            assert!(state
                .windows
                .iter()
                .any(|w| w.app_id == "feather-terminal" && w.is_visible()));
            eprintln!("[feather-launchpad-check] click closed Launchpad and mapped the app");
        })?;
    }
    if std::env::var("FEATHER_DEBUG_DND").as_deref() == Ok("resize-all") {
        let initial_id = Rc::new(Cell::new(None));
        let origin = Rc::new(Cell::new((0.0, 0.0)));
        // Four edges and four corners, shrinking then growing, with real client
        // dispatch/render opportunities between pointer movements.
        for pass in 0..2 {
            for edge in 0..8 {
                let base = 3000 + (pass * 8 + edge) * 600;
                let id = initial_id.clone();
                let press_origin = origin.clone();
                at(state, base, move |state| {
                    let w = state
                        .windows
                        .iter()
                        .find(|w| w.app_id == "feather-terminal")
                        .expect("terminal disappeared");
                    assert!(w.is_visible(), "terminal hidden before resize");
                    assert_eq!(id.get().unwrap_or(w.id), w.id, "terminal was replaced");
                    id.set(Some(w.id));
                    let (l, r, t, b) = (
                        w.x as f64 + 2.0,
                        (w.x + w.width) as f64 - 2.0,
                        w.y as f64 + 2.0,
                        (w.y + w.total_height()) as f64 - 2.0,
                    );
                    let point = match edge {
                        0 => (r, (t + b) * 0.5),
                        1 => (l, (t + b) * 0.5),
                        2 => ((l + r) * 0.5, t),
                        3 => ((l + r) * 0.5, b),
                        4 => (l, t),
                        5 => (r, t),
                        6 => (l, b),
                        _ => (r, b),
                    };
                    press_origin.set(point);
                    state.debug_pointer_motion(point.0, point.1);
                    state.debug_pointer_button(0x110, true);
                })?;
                for step in 1..=8 {
                    let origin = origin.clone();
                    at(state, base + 80 + step * 30, move |state| {
                        assert!(
                            state.active_drag.is_some(),
                            "client did not start edge resize {edge}"
                        );
                        let direction = if pass == 0 { 1.0 } else { -1.0 };
                        let delta = step as f64 * 8.0 * direction;
                        let (dx, dy) = match edge {
                            0 => (-delta, 0.0),
                            1 => (delta, 0.0),
                            2 => (0.0, delta),
                            3 => (0.0, -delta),
                            4 => (delta, delta),
                            5 => (-delta, delta),
                            6 => (delta, -delta),
                            _ => (-delta, -delta),
                        };
                        let p = origin.get();
                        state.debug_pointer_motion(p.0 + dx, p.1 + dy);
                    })?;
                }
                let id = initial_id.clone();
                at(state, base + 420, move |state| {
                    state.debug_pointer_button(0x110, false);
                    let w = state
                        .windows
                        .iter()
                        .find(|w| Some(w.id) == id.get())
                        .expect("terminal destroyed during resize");
                    assert!(w.is_visible(), "terminal hidden during resize");
                    assert!(
                        state.animations.window(w).identity(),
                        "resize restarted appearance animation"
                    );
                    eprintln!("[feather-resize-check] pass={pass} edge={edge} id={} rect={},{},{}x{} mapped={} minimized={}", w.id,w.x,w.y,w.width,w.height,w.mapped,w.minimized);
                })?;
            }
        }
        at(state, 12800, move |state| {
            let w = state
                .windows
                .iter_mut()
                .find(|w| w.app_id == "feather-terminal")
                .expect("terminal disappeared");
            // At scale 2 this is 2,568,432 bytes, a slot with 16 bytes of padding.
            w.resize_client(733, 219);
        })?;
        at(state, 13600, move |state| {
            let w = state
                .windows
                .iter()
                .find(|w| Some(w.id) == initial_id.get())
                .expect("terminal destroyed at unaligned SHM size");
            assert!(w.is_visible());
            assert_eq!((w.width, w.height), (733, 219));
            eprintln!("[feather-resize-check] unaligned SHM size passed");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_DOCK").is_some() {
        let app = state
            .app_registry
            .index_of("konsole")
            .expect("terminal registry entry");
        let slow = std::env::var_os("FEATHER_DEBUG_DOCK_SLOW").is_some();
        if slow {
            state.app_registry.apps[app].action = crate::apps::AppAction::Exec(format!(
                "sleep 0.8; exec {}",
                crate::apps::terminal_command()
            ));
        }
        let point = Rc::new(Cell::new((0.0, 0.0)));
        at(state, 2400, move |state| state.launch_app_instance(app))?;
        at(state, 2500, move |state| {
            assert!(
                state.shell.bounce.is_none(),
                "bounce started before the delay"
            );
        })?;
        at(state, 2800, move |state| {
            assert_eq!(
                state.shell.bounce.is_some(),
                slow,
                "fast/slow launch bounce decision"
            );
            eprintln!(
                "[feather-dock-check] slow={slow} bounce={}",
                state.shell.bounce.is_some()
            );
        })?;
        let p = point.clone();
        at(state, 3400, move |state| {
            let (x, y, size) = state
                .shell
                .dock_slot_global(DockTarget::App(app))
                .expect("terminal Dock slot");
            state.debug_pointer_motion((x + size * 0.5) as f64, (y + size * 0.5) as f64);
        })?;
        at(state, 3600, move |state| {
            let (x, y, size) = state.shell.dock_slot_global(DockTarget::App(app)).unwrap();
            p.set(((x + size * 0.5) as f64, (y + size * 0.5) as f64));
        })?;
        for (ms, minimized) in [(3800, true), (4400, false), (5000, true), (5600, false)] {
            let p = point.clone();
            at(state, ms, move |state| {
                let (x, y) = p.get();
                assert_eq!(
                    state.shell.dock_target_at(x, y).map(|(t, _)| t),
                    Some(DockTarget::App(app)),
                    "Dock moved away from the pointer"
                );
                state.debug_pointer_motion(x, y);
                state.debug_pointer_button(0x110, true);
                state.debug_pointer_button(0x110, false);
                assert!(state
                    .windows
                    .iter()
                    .filter(|w| w.app_id == "feather-terminal")
                    .all(|w| w.minimized == minimized));
                eprintln!("[feather-dock-check] same-point click minimized={minimized}");
            })?;
        }
        at(state, 6200, move |state| {
            let (x, y, size) = state.shell.dock_slot_global(DockTarget::App(app)).unwrap();
            state.debug_pointer_motion((x + size * 0.5) as f64, (y + size * 0.5) as f64);
            state.debug_pointer_button(0x112, true);
            state.debug_pointer_button(0x112, false);
        })?;
        at(state, 7200, move |state| {
            assert_eq!(
                state
                    .windows
                    .iter()
                    .filter(|w| w.app_id == "feather-terminal" && w.mapped)
                    .count(),
                2
            );
            state.execute_shell_action(ShellClickAction::DockMenu(
                DockMenuAction::NewWindow,
                DockTarget::App(app),
            ));
        })?;
        at(state, 8200, move |state| {
            assert_eq!(
                state
                    .windows
                    .iter()
                    .filter(|w| w.app_id == "feather-terminal" && w.mapped)
                    .count(),
                3
            );
            let mut visited = Vec::new();
            for _ in 0..3 {
                visited.push(
                    state
                        .windows
                        .iter()
                        .rev()
                        .find(|w| w.is_visible())
                        .unwrap()
                        .id,
                );
                state.execute_shell_action(ShellClickAction::DockPress(DockTarget::App(app)));
            }
            visited.sort_unstable();
            visited.dedup();
            assert_eq!(visited.len(), 3, "Dock click did not cycle all windows");
            let ids: Vec<_> = state
                .windows
                .iter()
                .filter(|w| w.app_id == "feather-terminal" && w.mapped)
                .map(|w| w.id)
                .collect();
            for id in ids {
                state.execute_shell_action(ShellClickAction::DockMenu(
                    DockMenuAction::Window(id),
                    DockTarget::App(app),
                ));
                let top = state.windows.iter().rev().find(|w| w.is_visible()).unwrap();
                assert_eq!(
                    top.id, id,
                    "Dock menu activated the app instead of the selected window"
                );
                assert_eq!(state.keyboard.current_focus().as_ref(), top.wl_surface());
            }
            let (x, y, size) = state.shell.dock_slot_global(DockTarget::App(app)).unwrap();
            state.open_dock_menu_for(DockTarget::App(app), R::new(x, y, size, size));
            let menu = state.shell.dock_menu.as_ref().unwrap();
            assert_eq!(
                menu.items
                    .iter()
                    .filter(|(_, a)| matches!(a, DockMenuAction::Window(_)))
                    .count(),
                3
            );
            eprintln!("[feather-dock-check] three windows, cycling, new-window menu and middle click passed");
        })?;
        at(state, 8400, move |state| {
            let r = state.shell.dock_menu.as_ref().unwrap().rect;
            state.debug_pointer_motion((r.x + r.w * 0.5) as f64, (r.y + r.h * 0.5) as f64);
        })?;
        at(state, 9000, move |state| {
            let (_, y, _) = state.shell.dock_slot_global(DockTarget::App(app)).unwrap();
            let r = state.shell.dock_menu.as_ref().unwrap().rect;
            assert!(
                (r.y + r.h + 10.0 - y).abs() < 0.01,
                "menu detached from Dock icon"
            );
            eprintln!("[feather-dock-check] menu anchored after magnification disappeared");
        })?;
        at(state, 9400, move |state| {
            state.shell.close_menus();
            state.debug_pointer_motion(20.0, 60.0);
            assert!(state.shell.bounce.is_none(), "Dock bounce did not finish");
        })?;
        at(state, 10000, move |state| {
            state.shell.open_switcher(state.switcher_items(), false);
            state.shell.switcher.as_mut().unwrap().visible = true;
            state.shell.switcher_dirty = true;
            state.needs_redraw = true;
        })?;
        at(state, 10500, move |state| {
            let sw = state.shell.switcher.as_ref().unwrap();
            let (index, key) = sw
                .items
                .iter()
                .enumerate()
                .find(|(_, i)| Some(i.key) != state.focused_app())
                .map(|(i, a)| (i, a.key))
                .unwrap();
            let r = state.shell.switcher_rect;
            let cell =
                ((state.shell.screen_w as f32 - 108.0) / sw.items.len() as f32).clamp(40.0, 96.0);
            state.debug_pointer_motion(
                (r.x + 14.0 + cell * (index as f32 + 0.5)) as f64,
                (r.y + 14.0 + cell * 0.5) as f64,
            );
            state.debug_pointer_button(0x110, true);
            state.debug_pointer_button(0x110, false);
            assert!(state.shell.switcher.is_none());
            assert_eq!(state.focused_app(), Some(key));
            eprintln!("[feather-dock-check] Alt+Tab icon click activated the chosen app");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_ACCESSIBILITY").is_some() {
        use crate::system_settings::{Action as A, Page};
        fn click(state: &mut FeatherCompositor, predicate: impl Fn(&A) -> bool) {
            let w = state
                .windows
                .iter()
                .find(|w| w.app_id == "builtin.settings")
                .unwrap();
            let (x, y) = state
                .system_settings
                .debug_point(
                    w.width as f32,
                    w.height as f32,
                    &state.settings_context(),
                    predicate,
                )
                .expect("accessibility control");
            let point = (
                w.x as f64 + x as f64,
                (w.y + w.titlebar_h()) as f64 + y as f64,
            );
            state.debug_pointer_motion(point.0, point.1);
            state.debug_pointer_button(0x110, true);
            state.debug_pointer_button(0x110, false);
        }
        at(state, 900, |s| {
            s.apply_system_setting(A::Page(Page::Accessibility));
        })?;
        at(state, 1300, |s| {
            assert!(!crate::theme::high_contrast());
            assert!(!crate::theme::reduce_transparency());
            assert!(!s.sticky_keys);
            click(s, |a| matches!(a, A::HighContrast));
            click(s, |a| matches!(a, A::ReduceTransparency));
            click(s, |a| matches!(a, A::StickyKeys));
            click(s, |a| matches!(a, A::TextSize(crate::theme::TextSize::Large)));
            assert!(crate::theme::high_contrast());
            assert!(crate::theme::reduce_transparency());
            assert!(s.sticky_keys);
            assert_eq!(crate::theme::text_size(), crate::theme::TextSize::Large);
            assert_eq!(crate::theme::palette().glass_fill.alpha(), 1.0);
            assert_eq!(crate::theme::palette().dock_fill.alpha(), 1.0);
        })?;
        at(state, 1800, |s| {
            s.open_or_focus_builtin(crate::apps::BuiltinWindowKind::QuickNotes);
            s.notes_buffer.clear();
            // Tap Left Shift (evdev 42): latches Shift on release
            s.debug_keyboard_key(42, true);
            s.debug_keyboard_key(42, false);
            assert_eq!(s.latched_modifiers.len(), 1);
            assert!(s.keyboard.modifier_state().shift);
            // Press 'a' (evdev 30): types 'A' and unlatches Shift
            s.debug_keyboard_key(30, true);
            s.debug_keyboard_key(30, false);
            assert!(s.latched_modifiers.is_empty());
            assert!(!s.keyboard.modifier_state().shift);
            // Press 'a' again: types 'a'
            s.debug_keyboard_key(30, true);
            s.debug_keyboard_key(30, false);
            // Tap Left Shift twice: latches on first tap, unlatches on second tap
            s.debug_keyboard_key(42, true);
            s.debug_keyboard_key(42, false);
            assert_eq!(s.latched_modifiers.len(), 1);
            s.debug_keyboard_key(42, true);
            s.debug_keyboard_key(42, false);
            assert!(s.latched_modifiers.is_empty());
            assert!(!s.keyboard.modifier_state().shift);
            // Press 'b' (evdev 48): types 'b'
            s.debug_keyboard_key(48, true);
            s.debug_keyboard_key(48, false);
            assert_eq!(s.notes_buffer, "Aab");
            let saved = crate::settings::Settings::load();
            assert!(saved.high_contrast);
            assert!(saved.reduce_transparency);
            assert!(saved.sticky_keys);
            assert_eq!(saved.text_size, crate::theme::TextSize::Large);
            s.windows.retain(|w| w.app_id != "builtin.notes");
            s.refresh_focus();
            eprintln!("[feather-accessibility-check] high contrast, reduce transparency, large text and sticky keys passed");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_LANGUAGE").is_some() {
        use crate::system_settings::{Action as A, Page};
        use crate::theme::{Lang, Region};
        fn click(state: &mut FeatherCompositor, predicate: impl Fn(&A) -> bool) {
            let w = state
                .windows
                .iter()
                .find(|w| w.app_id == "builtin.settings")
                .unwrap();
            let (x, y) = state
                .system_settings
                .debug_point(
                    w.width as f32,
                    w.height as f32,
                    &state.settings_context(),
                    predicate,
                )
                .expect("language control");
            let point = (
                w.x as f64 + x as f64,
                (w.y + w.titlebar_h()) as f64 + y as f64,
            );
            state.debug_pointer_motion(point.0, point.1);
            state.debug_pointer_button(0x110, true);
            state.debug_pointer_button(0x110, false);
        }
        at(state, 900, |s| {
            s.apply_system_setting(A::Page(Page::Language));
        })?;
        at(state, 1400, |s| {
            assert_eq!(crate::theme::lang(), Lang::Ru);
            assert_eq!(crate::theme::region(), Region::Ru);
            assert!(crate::theme::use_24h_time());
            assert!(crate::theme::first_weekday_monday());
            assert!(crate::theme::metric_units());
            for lang in [Lang::Es, Lang::De, Lang::Fr, Lang::Zh, Lang::En] {
                click(s, |a| matches!(a, A::Language(l) if *l == lang));
                assert_eq!(crate::theme::lang(), lang);
            }
            assert_eq!(feather_ui::i18n::tr("Системные настройки"), "System Settings");
            assert!(s.app_registry.apps.iter().any(|a| a.id == "builtin.settings" && a.name == "System Settings"));
            click(s, |a| matches!(a, A::RegionStep(1)));
            assert_eq!(crate::theme::region(), Region::Us);
            click(s, |a| matches!(a, A::Use24HourTime));
            assert!(!crate::theme::use_24h_time());
            assert!(s.shell.last_clock_str.ends_with("AM") || s.shell.last_clock_str.ends_with("PM"));
            click(s, |a| matches!(a, A::FirstWeekdayMonday(false)));
            assert!(!crate::theme::first_weekday_monday());
            click(s, |a| matches!(a, A::MetricUnits(false)));
            assert!(!crate::theme::metric_units());
        })?;
        at(state, 1900, |_s| {
            let saved = crate::settings::Settings::load();
            assert_eq!(saved.language, Lang::En);
            assert_eq!(saved.region, Region::Us);
            assert!(!saved.use_24h_time);
            assert!(!saved.first_weekday_monday);
            assert!(!saved.metric_units);
            eprintln!("[feather-language-check] language, region, 12/24h clock, weekday and units passed");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_SCREENSHOT_SETTINGS").is_some() {
        use crate::screenshot::Format;
        use crate::system_settings::{Action as A, Page};
        fn click(state: &mut FeatherCompositor, predicate: impl Fn(&A) -> bool) {
            let w = state
                .windows
                .iter()
                .find(|w| w.app_id == "builtin.settings")
                .unwrap();
            let (x, y) = state
                .system_settings
                .debug_point(
                    w.width as f32,
                    w.height as f32,
                    &state.settings_context(),
                    predicate,
                )
                .expect("screenshot settings control");
            let point = (
                w.x as f64 + x as f64,
                (w.y + w.titlebar_h()) as f64 + y as f64,
            );
            state.debug_pointer_motion(point.0, point.1);
            state.debug_pointer_button(0x110, true);
            state.debug_pointer_button(0x110, false);
        }
        let target_dir = std::env::var_os("FEATHER_PICKER_TEST_FILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("feather-shots-e2e"));
        let _ = std::fs::create_dir_all(&target_dir);
        let has_picker = std::env::var_os("FEATHER_PICKER_TEST_FILE").is_some();
        let dir_step = target_dir.clone();
        at(state, 700, move |s| {
            s.apply_system_setting(A::Page(Page::Desktop));
            s.system_settings.scroll = 320.0;
            s.mark_settings_dirty();
            assert_eq!(crate::screenshot::format(), Format::Png);
            assert!(crate::screenshot::custom_dir().is_none());
            if has_picker {
                click(s, |a| matches!(a, A::ChooseScreenshotDir));
                assert!(s.system_settings.screenshot_dir_busy);
            } else {
                crate::screenshot::set_custom_dir(Some(dir_step.clone()));
                s.save_settings();
                s.mark_settings_dirty();
            }
        })?;
        let dir_verify = target_dir.clone();
        at(state, 2100, move |s| {
            assert!(!s.system_settings.screenshot_dir_busy);
            assert_eq!(crate::screenshot::custom_dir(), Some(dir_verify.clone()));
            click(s, |a| matches!(a, A::ResetScreenshotDir));
            assert!(crate::screenshot::custom_dir().is_none());
            crate::screenshot::set_custom_dir(Some(dir_verify.clone()));
            s.save_settings();
            s.mark_settings_dirty();
            for fmt in [Format::Jpeg, Format::Png, Format::Webp] {
                click(s, |a| matches!(a, A::ScreenshotFormat(f) if *f == fmt));
                assert_eq!(crate::screenshot::format(), fmt);
            }
            s.take_screenshot(crate::screenshot::Target::Area {
                x: 80.0,
                y: 60.0,
                w: 240.0,
                h: 160.0,
                radius: 12.0,
            });
        })?;
        at(state, 2800, move |_s| {
            let saved = crate::settings::Settings::load();
            assert_eq!(saved.screenshot_format, Format::Webp);
            assert_eq!(saved.screenshot_dir, Some(target_dir.clone()));
            let files: Vec<_> = std::fs::read_dir(&target_dir)
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("webp"))
                .collect();
            assert!(!files.is_empty(), "expected saved .webp screenshot in {:?}", target_dir);
            let img = image::load_from_memory(&std::fs::read(&files[0]).unwrap()).unwrap();
            assert!(img.width() > 0 && img.height() > 0);
            eprintln!("[feather-screenshot-settings-check] folder picker, PNG/JPEG/WebP formats, capture and persistence passed");
        })?;
    }
    Ok(())
}
