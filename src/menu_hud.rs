//! Instant HUD search across the focused window's menu tree (`DBusMenu` / GTK `org.gtk.Menus`)
//! and Feather DE's own menu bar (`Приложение`, `Окно`, `Пёрышко`, `Справка`), inspired by
//! macOS Tahoe `Help → Search` and Unity HUD. Zero background memory or idle CPU cost: the menu
//! list is flattened on demand only while `SpotlightMode::MenuHud` is open.

use crate::appmenu::MenuNode;
use crate::apps::IconKind;
use crate::shell::{menu_rows, AppIcon, MenuContext, MenuRow, ShellClickAction, TopMenu};
use crate::spotlight::Hit;
use crate::FeatherCompositor;

const MAX_HUD_HITS: usize = 48;
const MAX_MENU_DEPTH: usize = 8;
const MAX_MENU_ITEMS: usize = 512;

#[derive(Debug, Clone)]
pub struct MenuCommand {
    /// Clean label without leading checkmark, used for prefix/exact scoring
    pub clean_label: String,
    /// Displayed label (with `✓ ` / `● ` when checked)
    pub display_label: String,
    /// Full menu path (`Файл ▸ Экспорт`)
    pub breadcrumb: String,
    /// Formatted shortcut (`⌘⇧S`)
    pub shortcut: String,
    /// Extra lowercase search terms (original Russian label when translated, etc.)
    pub search_extra: String,
    pub icon: AppIcon,
    pub action: ShellClickAction,
}

/// Flattens an exported client `MenuNode` tree into actionable leaf commands with breadcrumbs.
pub fn collect_client_menu(
    root: &MenuNode,
    generation: u64,
    icon: &AppIcon,
    out: &mut Vec<MenuCommand>,
) {
    let mut path: Vec<String> = Vec::with_capacity(MAX_MENU_DEPTH);
    walk_client_node(root, generation, icon, &mut path, 0, out);
}

fn walk_client_node(
    node: &MenuNode,
    generation: u64,
    icon: &AppIcon,
    path: &mut Vec<String>,
    depth: usize,
    out: &mut Vec<MenuCommand>,
) {
    if depth > MAX_MENU_DEPTH || out.len() >= MAX_MENU_ITEMS {
        return;
    }
    for child in &node.children {
        if out.len() >= MAX_MENU_ITEMS {
            break;
        }
        if !child.visible || child.separator {
            continue;
        }
        let label = child.label.trim();
        if label.is_empty() {
            continue;
        }
        if child.submenu || !child.children.is_empty() {
            if !child.enabled {
                continue;
            }
            path.push(label.to_string());
            walk_client_node(child, generation, icon, path, depth + 1, out);
            path.pop();
        } else if child.enabled {
            let display_label = if child.checked {
                format!("{}{label}", if child.radio { "● " } else { "✓ " })
            } else {
                label.to_string()
            };
            out.push(MenuCommand {
                clean_label: label.to_string(),
                display_label,
                breadcrumb: path.join(" ▸ "),
                shortcut: child.shortcut.clone(),
                search_extra: String::new(),
                icon: icon.clone(),
                action: ShellClickAction::AppMenuActivate {
                    generation,
                    id: child.id,
                },
            });
        }
    }
}

/// Collects submenu IDs that have not loaded children yet (`AboutToShow` lazy population in Qt).
pub fn lazy_submenu_ids(root: &MenuNode) -> Vec<i32> {
    let mut ids = Vec::new();
    for top in &root.children {
        if !top.visible || !top.enabled || top.separator {
            continue;
        }
        if top.submenu && top.children.is_empty() && ids.len() < 16 {
            ids.push(top.id);
        }
        for sub in &top.children {
            if sub.visible && sub.enabled && !sub.separator && sub.submenu && sub.children.is_empty() && ids.len() < 16 {
                ids.push(sub.id);
            }
        }
    }
    ids
}

/// Appends Feather DE's built-in menu bar commands (`Приложение`, `Окно`, `Пёрышко`, `Справка`).
pub fn collect_shell_menus(
    ctx: &MenuContext,
    app_icon: Option<&AppIcon>,
    out: &mut Vec<MenuCommand>,
) {
    let menus = [TopMenu::App, TopMenu::Window, TopMenu::Feather, TopMenu::Help];
    for menu in menus {
        let menu_title = match menu {
            TopMenu::App => {
                if ctx.app_name.is_empty() {
                    "Пёрышко".to_string()
                } else {
                    ctx.app_name.clone()
                }
            }
            TopMenu::Window => feather_ui::i18n::tr("Окно").into_owned(),
            TopMenu::Feather => "Пёрышко".to_string(),
            TopMenu::Help => feather_ui::i18n::tr("Справка").into_owned(),
            _ => continue,
        };
        let icon = match menu {
            TopMenu::App | TopMenu::Window => app_icon.cloned().unwrap_or(AppIcon {
                kind: if matches!(menu, TopMenu::Window) {
                    IconKind::Launchpad
                } else {
                    IconKind::GenericApp
                },
                name: None,
            }),
            _ => AppIcon {
                kind: IconKind::Settings,
                name: None,
            },
        };
        for row in menu_rows(menu, ctx) {
            let MenuRow::Item {
                label,
                shortcut,
                action: Some(action),
                checked,
                radio,
                ..
            } = row
            else {
                continue;
            };
            if matches!(action, ShellClickAction::OpenMenuHud) {
                continue;
            }
            let translated = feather_ui::i18n::tr(&label).into_owned();
            let display_label = if checked {
                format!("{}{translated}", if radio { "● " } else { "✓ " })
            } else {
                translated.clone()
            };
            out.push(MenuCommand {
                clean_label: translated,
                display_label,
                breadcrumb: menu_title.clone(),
                shortcut,
                search_extra: label.to_lowercase(),
                icon: icon.clone(),
                action,
            });
        }
    }
}

/// Scores a menu command against a lowercase trimmed query (`0` = no match, higher = better).
pub fn score_menu_command(cmd: &MenuCommand, query: &str) -> u8 {
    let q = query.trim();
    if q.is_empty() {
        return 1;
    }
    let label = cmd.clean_label.to_lowercase();
    let crumb = cmd.breadcrumb.to_lowercase();
    let all_match = q.split_whitespace().all(|w| {
        label.contains(w) || crumb.contains(w) || cmd.search_extra.contains(w)
    });
    if !all_match {
        return 0;
    }
    if label == q {
        5
    } else if label.starts_with(q) {
        4
    } else if label.split_whitespace().any(|part| part.starts_with(q)) {
        3
    } else if label.contains(q) {
        2
    } else {
        1
    }
}

/// Converts between QWERTY and Russian ЙЦУКЕН keyboard layouts so mistyped queries still match.
pub fn flip_keyboard_layout(input: &str) -> Option<String> {
    const PAIRS: &[(char, char)] = &[
        ('q', 'й'), ('w', 'ц'), ('e', 'у'), ('r', 'к'), ('t', 'е'), ('y', 'н'),
        ('u', 'г'), ('i', 'ш'), ('o', 'щ'), ('p', 'з'), ('[', 'х'), (']', 'ъ'),
        ('a', 'ф'), ('s', 'ы'), ('d', 'в'), ('f', 'а'), ('g', 'п'), ('h', 'р'),
        ('j', 'о'), ('k', 'л'), ('l', 'д'), (';', 'ж'), ('\'', 'э'),
        ('z', 'я'), ('x', 'ч'), ('c', 'с'), ('v', 'м'), ('b', 'и'), ('n', 'т'),
        ('m', 'ь'), (',', 'б'), ('.', 'ю'), ('`', 'ё'),
    ];
    let mut changed = false;
    let mut out = String::with_capacity(input.len() * 2);
    for ch in input.chars() {
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        if let Some(&(_, ru)) = PAIRS.iter().find(|&&(en, _)| en == lower) {
            changed = true;
            out.push(ru);
        } else if let Some(&(en, _)) = PAIRS.iter().find(|&&(_, ru)| ru == lower) {
            changed = true;
            out.push(en);
        } else {
            out.push(lower);
        }
    }
    changed.then_some(out)
}

pub fn filter_menu_commands(commands: Vec<MenuCommand>, query: &str) -> Vec<Hit> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return commands
            .into_iter()
            .take(MAX_HUD_HITS)
            .map(into_hit)
            .collect();
    }
    let mut scored = score_all(&commands, &q);
    if scored.is_empty() {
        if let Some(flipped) = flip_keyboard_layout(&q) {
            scored = score_all(&commands, &flipped);
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored
        .into_iter()
        .take(MAX_HUD_HITS)
        .map(|(_, cmd)| into_hit(cmd.clone()))
        .collect()
}

fn score_all<'a>(commands: &'a [MenuCommand], query: &str) -> Vec<(u8, &'a MenuCommand)> {
    commands
        .iter()
        .filter_map(|cmd| {
            let s = score_menu_command(cmd, query);
            (s > 0).then_some((s, cmd))
        })
        .collect()
}

fn into_hit(cmd: MenuCommand) -> Hit {
    Hit {
        name: cmd.display_label,
        sub: cmd.breadcrumb,
        icon: cmd.icon,
        emoji: None,
        badge: cmd.shortcut,
        action: cmd.action,
    }
}

impl FeatherCompositor {
    pub fn prefetch_menu_hud(&self) {
        if let Some(root) = self.shell.client_menu.as_deref() {
            let ids = lazy_submenu_ids(root);
            if !ids.is_empty() {
                self.appmenus.prefetch_submenus(self.shell.client_generation, &ids);
            }
        }
    }

    pub fn menu_hud_hits(&self, query: &str) -> Vec<Hit> {
        let top = self
            .windows
            .iter()
            .rev()
            .find(|w| w.is_visible() && !w.pip_ghost && !w.larvis);
        let app_icon = top.map(|w| {
            self.app_registry
                .app_for_app_id(&w.app_id)
                .map(|i| AppIcon::of(&self.app_registry.apps[i]))
                .unwrap_or(AppIcon {
                    kind: IconKind::GenericApp,
                    name: None,
                })
        });
        let mut commands = Vec::with_capacity(64);
        if let Some(root) = self.shell.client_menu.as_deref() {
            let icon = app_icon.clone().unwrap_or(AppIcon {
                kind: IconKind::GenericApp,
                name: None,
            });
            collect_client_menu(root, self.shell.client_generation, &icon, &mut commands);
        }
        let ctx = self.build_menu_context();
        collect_shell_menus(&ctx, app_icon.as_ref(), &mut commands);
        filter_menu_commands(commands, query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i32, label: &str, shortcut: &str, enabled: bool, children: Vec<MenuNode>) -> MenuNode {
        MenuNode {
            id,
            label: label.into(),
            shortcut: shortcut.into(),
            enabled,
            visible: true,
            separator: false,
            checked: false,
            radio: false,
            submenu: !children.is_empty(),
            children,
        }
    }

    #[test]
    fn flattens_nested_menus_with_breadcrumbs_and_skips_disabled_items() {
        let mut png = node(12, "Экспортировать как PNG…", "⇧⌘E", true, vec![]);
        png.checked = true;
        let disabled = node(13, "Отправить по факсу", "", false, vec![]);
        let export = node(11, "Экспорт", "", true, vec![png, disabled]);
        let save = node(10, "Сохранить", "⌘S", true, vec![]);
        let file = node(1, "Файл", "", true, vec![save, export]);
        let root = node(0, "", "", true, vec![file]);

        let icon = AppIcon {
            kind: IconKind::GenericApp,
            name: None,
        };
        let mut out = Vec::new();
        collect_client_menu(&root, 42, &icon, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].display_label, "Сохранить");
        assert_eq!(out[0].breadcrumb, "Файл");
        assert_eq!(out[0].shortcut, "⌘S");
        assert_eq!(out[1].display_label, "✓ Экспортировать как PNG…");
        assert_eq!(out[1].breadcrumb, "Файл ▸ Экспорт");
        assert_eq!(out[1].shortcut, "⇧⌘E");
    }

    #[test]
    fn ranks_prefix_above_breadcrumb_and_recovers_wrong_keyboard_layout() {
        let icon = AppIcon {
            kind: IconKind::GenericApp,
            name: None,
        };
        let root = node(
            0,
            "",
            "",
            true,
            vec![
                node(
                    1,
                    "Файл",
                    "",
                    true,
                    vec![
                        node(2, "Открыть недавние", "", true, vec![]),
                        node(3, "Сохранить как…", "⇧⌘S", true, vec![]),
                    ],
                ),
                node(
                    4,
                    "Правка",
                    "",
                    true,
                    vec![node(5, "Сохранить в буфер", "", true, vec![])],
                ),
            ],
        );
        let mut cmds = Vec::new();
        collect_client_menu(&root, 1, &icon, &mut cmds);

        // Multi-word breadcrumb + item search
        let hits = filter_menu_commands(cmds.clone(), "файл сохр");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Сохранить как…");
        assert_eq!(hits[0].badge, "⇧⌘S");

        // Wrong keyboard layout ("cj[hfybnm rfr" -> "сохранить как")
        let flipped_hits = filter_menu_commands(cmds, "cj[hfybnm rfr");
        assert_eq!(flipped_hits.len(), 1);
        assert_eq!(flipped_hits[0].name, "Сохранить как…");
    }
}
