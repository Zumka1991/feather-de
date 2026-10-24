//! GTK's exported menus (`org.gtk.Menus`) and actions (`org.gtk.Actions`), turned into the
//! same `MenuNode` tree as DBusMenu. Runs on the appmenu worker only.
//!
//! A GMenuModel is exported as groups of menus: `Start([group…])` returns
//! `(group, menu, [item…])`, items link other menus through `:section` and `:submenu`
//! (`(group, menu)`). Sections are drawn inline between separators. Items name an action
//! (`app.quit`, `win.save`) and optionally a target. Enabled/checked state comes from
//! `DescribeAll` of the application and window action groups.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use zbus::{
    zvariant::{OwnedValue, Signature, Value},
    Connection, Proxy,
};

use crate::appmenu::{label_without_mnemonic, GtkActions, MenuNode};

pub const MENUS: &str = "org.gtk.Menus";
const ACTIONS: &str = "org.gtk.Actions";
const MAX_ITEMS: usize = 4096;
const MAX_GROUPS: usize = 256;
const MAX_DEPTH: usize = 12;

type Item = HashMap<String, OwnedValue>;
type Menus = HashMap<(u32, u32), Vec<Item>>;
type States = HashMap<String, (bool, Vec<OwnedValue>)>;

/// What a leaf activates
#[derive(Debug)]
pub struct Action {
    window: bool,
    name: String,
    target: Option<OwnedValue>,
}

#[derive(Debug, Default)]
pub struct GtkMenu {
    pub root: Option<Arc<MenuNode>>,
    actions: HashMap<i32, Action>,
    /// Subscribed groups, ended when the window loses focus
    pub groups: Vec<u32>,
}

fn link(item: &Item, key: &str) -> Option<(u32, u32)> {
    item.get(key).and_then(|v| v.try_clone().ok()).and_then(|v| <(u32, u32)>::try_from(v).ok())
}

fn string<'a>(item: &'a Item, key: &str) -> Option<&'a str> {
    item.get(key).and_then(|v| <&str>::try_from(v).ok())
}

/// Subscribes to group 0 and every group it links to; GTK sends `Changed` for them
async fn fetch(menus: &Proxy<'_>) -> zbus::Result<(Menus, Vec<u32>)> {
    let mut all = Menus::new();
    let mut subscribed = Vec::new();
    let mut pending = vec![0u32];
    let mut items = 0;
    while !pending.is_empty() && subscribed.len() < MAX_GROUPS {
        subscribed.extend(&pending);
        let reply: Vec<(u32, u32, Vec<Item>)> = menus.call("Start", &(std::mem::take(&mut pending),)).await?;
        for (group, menu, list) in reply {
            items += list.len();
            if items > MAX_ITEMS {
                return Err(zbus::Error::Failure("GTK menu exceeds limits".into()));
            }
            for item in &list {
                for (g, _) in [link(item, ":section"), link(item, ":submenu")].into_iter().flatten() {
                    if !subscribed.contains(&g) && !pending.contains(&g) {
                        pending.push(g);
                    }
                }
            }
            all.insert((group, menu), list);
        }
    }
    Ok((all, subscribed))
}

async fn describe(conn: &Connection, service: &str, path: Option<&str>) -> Option<States> {
    let path = path?;
    let proxy = Proxy::new(conn, service.to_owned(), path.to_owned(), ACTIONS).await.ok()?;
    let all: HashMap<String, (bool, Signature, Vec<OwnedValue>)> = proxy.call("DescribeAll", &()).await.ok()?;
    Some(all.into_iter().map(|(name, (enabled, _, state))| (name, (enabled, state))).collect())
}

/// `<Primary><Shift>n` → `⌃⇧N`
fn accel(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.strip_prefix('<') {
        let Some(end) = start.find('>') else { break };
        out.push_str(match &start[..end] {
            "Primary" | "Control" | "Ctrl" => "⌃",
            "Shift" => "⇧",
            "Alt" => "⌥",
            "Super" | "Meta" => "⌘",
            _ => "",
        });
        rest = &start[end + 1..];
    }
    let key: String = rest.chars().take(16).collect();
    out.push_str(&if key.chars().count() == 1 { key.to_uppercase() } else { key });
    out
}

struct Builder<'a> {
    menus: &'a Menus,
    app: Option<&'a States>,
    window: Option<&'a States>,
    next: i32,
    actions: HashMap<i32, Action>,
}

impl Builder<'_> {
    fn id(&mut self) -> i32 {
        self.next += 1;
        self.next
    }

    fn node(&mut self, label: String) -> MenuNode {
        MenuNode { id: self.id(), label, shortcut: String::new(), enabled: true, visible: true, separator: false, checked: false, radio: false, submenu: false, children: Vec::new() }
    }

    /// Items of one menu with its sections flattened between separators
    fn items(&mut self, key: (u32, u32), depth: usize, seen: &mut HashSet<(u32, u32)>) -> Vec<MenuNode> {
        let mut out: Vec<MenuNode> = Vec::new();
        if depth > MAX_DEPTH || !seen.insert(key) {
            return out;
        }
        let Some(list) = self.menus.get(&key) else { return out };
        for item in list {
            if let Some(section) = link(item, ":section") {
                let inner = self.items(section, depth + 1, seen);
                if inner.is_empty() { continue; }
                if out.last().is_some_and(|n| !n.separator) {
                    let mut sep = self.node(String::new());
                    sep.separator = true;
                    out.push(sep);
                }
                out.extend(inner);
                let mut sep = self.node(String::new());
                sep.separator = true;
                out.push(sep);
                continue;
            }
            let mut node = self.node(label_without_mnemonic(string(item, "label").unwrap_or("")));
            if let Some(sub) = link(item, ":submenu") {
                node.children = self.items(sub, depth + 1, seen);
                node.submenu = true;
            } else if let Some(full) = string(item, "action") {
                let (window, name) = match full.split_once('.') {
                    Some(("win", n)) => (true, n),
                    Some(("app", n)) => (false, n),
                    _ => { node.enabled = false; out.push(node); continue; }
                };
                let states = if window { self.window } else { self.app };
                let target = item.get("target").and_then(|v| v.try_clone().ok());
                match states.map(|s| s.get(name)) {
                    // A known group without this action: GTK shows it insensitive
                    Some(None) => node.enabled = false,
                    Some(Some((enabled, state))) => {
                        node.enabled = *enabled;
                        match (state.first(), &target) {
                            (Some(s), None) => {
                                if let Ok(b) = bool::try_from(s) { node.checked = b; }
                            }
                            (Some(s), Some(t)) => {
                                node.radio = true;
                                node.checked = **s == **t;
                            }
                            _ => {}
                        }
                    }
                    None => {}
                }
                node.shortcut = string(item, "accel").map(accel).unwrap_or_default();
                self.actions.insert(node.id, Action { window, name: name.chars().take(256).collect(), target });
            } else {
                node.enabled = false;
            }
            out.push(node);
        }
        // No leading/trailing separators
        while out.last().is_some_and(|n| n.separator) { out.pop(); }
        while out.first().is_some_and(|n| n.separator) { out.remove(0); }
        out
    }
}

pub async fn load(conn: &Connection, menus: &Proxy<'_>, paths: &GtkActions) -> zbus::Result<GtkMenu> {
    let service = menus.destination().to_string();
    let (all, groups) = fetch(menus).await?;
    let app = describe(conn, &service, paths.app.as_deref()).await;
    let window = describe(conn, &service, paths.window.as_deref()).await;
    Ok(build(&all, app.as_ref(), window.as_ref(), groups))
}

fn build(all: &Menus, app: Option<&States>, window: Option<&States>, groups: Vec<u32>) -> GtkMenu {
    let mut b = Builder { menus: all, app, window, next: 0, actions: HashMap::new() };
    let children = b.items((0, 0), 0, &mut HashSet::new());
    let root = MenuNode { id: 0, label: String::new(), shortcut: String::new(), enabled: true, visible: true, separator: false, checked: false, radio: false, submenu: true, children };
    GtkMenu { root: Some(Arc::new(root)), actions: b.actions, groups }
}

impl GtkMenu {
    pub async fn activate(&self, conn: &Connection, service: &str, paths: &GtkActions, id: i32) {
        let Some(action) = self.actions.get(&id) else { return };
        let Some(path) = (if action.window { paths.window.as_deref() } else { paths.app.as_deref() }) else { return };
        let Ok(proxy) = Proxy::new(conn, service.to_owned(), path.to_owned(), ACTIONS).await else { return };
        let parameter: Vec<Value<'_>> = action.target.iter().filter_map(|t| t.try_clone().ok()).map(Value::from).collect();
        let platform: HashMap<&str, Value<'_>> = HashMap::new();
        let _ = proxy.call_noreply("Activate", &(action.name.as_str(), parameter, platform)).await;
    }

    /// Unsubscribe when the window loses focus (GTK also forgets us if we vanish)
    pub async fn end(&self, menus: &Proxy<'_>) {
        if !self.groups.is_empty() {
            let _ = menus.call_noreply("End", &(self.groups.clone(),)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(pairs: &[(&str, Value<'static>)]) -> Item {
        pairs.iter().map(|(k, v)| (k.to_string(), OwnedValue::try_from(v.try_clone().unwrap()).unwrap())).collect()
    }

    #[test]
    fn sections_submenus_states_and_accels_become_menu_nodes() {
        let mut all = Menus::new();
        all.insert((0, 0), vec![
            item(&[("label", "_File".into()), (":submenu", Value::from((1u32, 0u32)))]),
            item(&[("label", "View".into()), (":submenu", Value::from((1u32, 1u32)))]),
        ]);
        all.insert((1, 0), vec![
            item(&[(":section", Value::from((2u32, 0u32)))]),
            item(&[(":section", Value::from((2u32, 1u32)))]),
        ]);
        all.insert((2, 0), vec![
            item(&[("label", "New".into()), ("action", "app.new".into()), ("accel", "<Primary>n".into())]),
            item(&[("label", "Save".into()), ("action", "win.save".into())]),
        ]);
        all.insert((2, 1), vec![item(&[("label", "Quit".into()), ("action", "app.quit".into()), ("accel", "<Primary><Shift>q".into())])]);
        all.insert((1, 1), vec![
            item(&[("label", "Sidebar".into()), ("action", "win.sidebar".into())]),
            item(&[("label", "Large".into()), ("action", "win.size".into()), ("target", "large".into())]),
            item(&[("label", "Small".into()), ("action", "win.size".into()), ("target", "small".into())]),
            item(&[("label", "Loop".into()), (":submenu", Value::from((1u32, 1u32)))]),
        ]);
        let ov = |v: Value<'static>| OwnedValue::try_from(v).unwrap();
        let app: States = [("new".into(), (true, vec![])), ("quit".into(), (true, vec![]))].into_iter().collect();
        let win: States = [
            ("save".into(), (false, vec![])),
            ("sidebar".into(), (true, vec![ov(Value::Bool(true))])),
            ("size".into(), (true, vec![ov("small".into())])),
        ].into_iter().collect();
        let m = build(&all, Some(&app), Some(&win), vec![0, 1, 2]);
        let root = m.root.unwrap();
        assert_eq!(root.children.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), ["File", "View"]);
        let file = &root.children[0];
        let labels: Vec<_> = file.children.iter().map(|n| if n.separator { "—" } else { n.label.as_str() }).collect();
        assert_eq!(labels, ["New", "Save", "—", "Quit"]);
        assert_eq!(file.children[0].shortcut, "⌃N");
        assert_eq!(file.children[3].shortcut, "⌃⇧Q");
        assert!(!file.children[1].enabled, "disabled action");
        let view = &root.children[1];
        assert!(view.children[0].checked && !view.children[0].radio);
        assert!(view.children[1].radio && !view.children[1].checked);
        assert!(view.children[2].radio && view.children[2].checked);
        assert!(view.children[3].children.is_empty(), "a menu linking itself does not loop");
        let quit = file.children[3].id;
        assert_eq!(m.actions[&quit].name, "quit");
        assert!(!m.actions[&quit].window);
    }

    #[test]
    fn unknown_actions_are_insensitive_and_accels_are_bounded() {
        let mut all = Menus::new();
        all.insert((0, 0), vec![item(&[("label", "M".into()), (":submenu", Value::from((0u32, 1u32)))])]);
        all.insert((0, 1), vec![
            item(&[("label", "Gone".into()), ("action", "app.gone".into())]),
            item(&[("label", "Odd".into()), ("action", "noprefix".into())]),
        ]);
        let app: States = HashMap::new();
        let m = build(&all, Some(&app), None, vec![0]);
        let menu = &m.root.unwrap().children[0];
        assert!(menu.children.iter().all(|n| !n.enabled));
        assert_eq!(accel("<Alt>F4"), "⌥F4");
        assert_eq!(accel("<Primary>comma"), "⌃comma");
    }
}
