//! Installing and removing programs through PackageKit on the system bus. PackageKit asks
//! polkit (Feather's own agent shows the password dialog), locks dpkg, downloads and reports
//! progress; the store only names the package. Each operation runs on its own thread.

use futures_lite::StreamExt;
use std::collections::HashMap;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, MatchRule, MessageStream, Proxy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Install,
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Overall fraction (None while PackageKit does not know) and what is happening
    Progress(Option<f32>, String),
    /// Removing would also take these packages away: the store asks first
    Confirm(Vec<String>),
    Done,
    /// The password dialog was dismissed or the action is not allowed
    Cancelled,
    Failed(String),
}

const DEST: &str = "org.freedesktop.PackageKit";

// PackageKit's enums (pk-enum.h); bitfields are 1 << value
const FILTER_INSTALLED: u64 = 1 << 2;
const FILTER_NOT_INSTALLED: u64 = 1 << 3;
const FILTER_NEWEST: u64 = 1 << 16;
const FILTER_ARCH: u64 = 1 << 18;
const FLAG_ONLY_TRUSTED: u64 = 1 << 1;
const FLAG_SIMULATE: u64 = 1 << 2;
const INFO_REMOVING: u32 = 13;
const EXIT_SUCCESS: u32 = 1;
const EXIT_CANCELLED: u32 = 3;

const ERROR_NO_NETWORK: u32 = 2;
const ERROR_PACKAGE_NOT_FOUND: u32 = 8;
const ERROR_PACKAGE_DOWNLOAD_FAILED: u32 = 10;
const ERROR_DEP_RESOLUTION_FAILED: u32 = 13;
const ERROR_TRANSACTION_CANCELLED: u32 = 17;
const ERROR_NO_CACHE: u32 = 18;
const ERROR_CANNOT_REMOVE_SYSTEM_PACKAGE: u32 = 20;
const ERROR_CANNOT_GET_LOCK: u32 = 26;
const ERROR_REPO_NOT_AVAILABLE: u32 = 37;
const ERROR_NO_MORE_MIRRORS: u32 = 43;
const ERROR_NO_SPACE: u32 = 46;
const ERROR_NOT_AUTHORIZED: u32 = 48;

enum Call {
    Resolve(u64, Vec<String>),
    Install(u64, Vec<String>),
    /// flags, ids, allow_deps, autoremove
    Remove(u64, Vec<String>, bool, bool),
    RefreshCache,
}

#[derive(Default)]
struct Outcome {
    exit: u32,
    error: Option<(u32, String)>,
    /// (info, package id) of every Package signal
    packages: Vec<(u32, String)>,
}

/// "gimp;3.0.4-1;amd64;ubuntu" → "gimp"
fn name_of(id: &str) -> &str {
    id.split(';').next().unwrap_or(id)
}

/// Runs `op` for `package` on a thread; `send` gets progress and then one final event.
/// `confirmed`: the user already agreed to what else a removal takes away.
pub fn run(op: Op, package: String, confirmed: bool, send: impl Fn(Event) + Send + 'static) {
    std::thread::spawn(move || {
        let event = futures_lite::future::block_on(work(op, &package, confirmed, &send)).unwrap_or_else(|e| match e {
            zbus::Error::MethodError(name, ..) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" => {
                Event::Failed("Служба PackageKit недоступна (пакет packagekit)".into())
            }
            e => Event::Failed(format!("PackageKit: {e}")),
        });
        send(event);
    });
}

async fn work(op: Op, package: &str, confirmed: bool, send: &(impl Fn(Event) + Send)) -> zbus::Result<Event> {
    let conn = Connection::system().await?;
    send(Event::Progress(None, "Подготовка…".into()));
    let progress = |fraction: Option<f32>, status: u32| {
        if let Some(text) = status_text(op, status) {
            send(Event::Progress(fraction, text.into()));
        }
    };
    let mut id = resolve(&conn, op, package).await?;
    // A catalog newer than apt's package lists (or no lists at all): refresh them once
    if id.is_none() && op == Op::Install {
        let refreshed = transaction(&conn, Call::RefreshCache, &progress).await?;
        if let Some(event) = failure(&refreshed) {
            return Ok(event);
        }
        id = resolve(&conn, op, package).await?;
    }
    let Some(id) = id else {
        return Ok(Event::Failed(match op {
            Op::Install => format!("Пакет {package} не найден в источниках программ"),
            Op::Remove => format!("Пакет {package} не установлен"),
        }));
    };
    match op {
        Op::Install => {
            let mut done = transaction(&conn, Call::Install(FLAG_ONLY_TRUSTED, vec![id.clone()]), &progress).await?;
            // Package lists older than the mirror: the files moved; refresh and try once more
            if matches!(done.error, Some((ERROR_PACKAGE_DOWNLOAD_FAILED | ERROR_PACKAGE_NOT_FOUND | ERROR_NO_CACHE, _))) {
                let refreshed = transaction(&conn, Call::RefreshCache, &progress).await?;
                if failure(&refreshed).is_none() {
                    if let Some(id) = resolve(&conn, op, package).await? {
                        done = transaction(&conn, Call::Install(FLAG_ONLY_TRUSTED, vec![id]), &progress).await?;
                    }
                }
            }
            Ok(failure(&done).unwrap_or(Event::Done))
        }
        Op::Remove => {
            if !confirmed {
                let simulated = transaction(&conn, Call::Remove(FLAG_SIMULATE, vec![id.clone()], true, false), &|_, _| {}).await?;
                if let Some(event) = failure(&simulated) {
                    return Ok(event);
                }
                let others = also_removed(&simulated.packages, package);
                if !others.is_empty() {
                    return Ok(Event::Confirm(others));
                }
            }
            let done = transaction(&conn, Call::Remove(0, vec![id], true, false), &progress).await?;
            Ok(failure(&done).unwrap_or(Event::Done))
        }
    }
}

/// The package id PackageKit knows for this name: the newest installable one, or the
/// installed one for a removal
async fn resolve(conn: &Connection, op: Op, package: &str) -> zbus::Result<Option<String>> {
    let filter = match op {
        Op::Install => FILTER_NOT_INSTALLED | FILTER_NEWEST | FILTER_ARCH,
        Op::Remove => FILTER_INSTALLED,
    };
    let found = transaction(conn, Call::Resolve(filter, vec![package.to_string()]), &|_, _| {}).await?;
    Ok(found.packages.into_iter().map(|(_, id)| id).find(|id| name_of(id) == package))
}

/// Other packages a simulated removal takes away
fn also_removed(packages: &[(u32, String)], package: &str) -> Vec<String> {
    let mut names: Vec<String> = packages
        .iter()
        .filter(|(info, id)| *info == INFO_REMOVING && name_of(id) != package)
        .map(|(_, id)| name_of(id).to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// One PackageKit transaction: create it, listen to its signals, call `call`, wait for Finished
async fn transaction(conn: &Connection, call: Call, progress: &dyn Fn(Option<f32>, u32)) -> zbus::Result<Outcome> {
    let pk = Proxy::new(conn, DEST, "/org/freedesktop/PackageKit", "org.freedesktop.PackageKit").await?;
    let path: OwnedObjectPath = pk.call("CreateTransaction", &()).await?;
    // Subscribed before the call, so no signal of this transaction is missed
    let rule = MatchRule::builder().msg_type(zbus::message::Type::Signal).path(path.clone())?.build();
    let mut stream = MessageStream::for_match_rule(rule, conn, Some(256)).await?;
    let tx = Proxy::new(conn, DEST, path, "org.freedesktop.PackageKit.Transaction").await?;
    let locale = std::env::var("LC_ALL").ok().filter(|l| !l.is_empty()).or_else(|| std::env::var("LANG").ok()).unwrap_or_else(|| "C.UTF-8".into());
    tx.call_method("SetHints", &(vec![format!("locale={locale}"), "interactive=true".to_string()],)).await?;
    match call {
        Call::Resolve(filter, names) => tx.call_method("Resolve", &(filter, names)).await?,
        Call::Install(flags, ids) => tx.call_method("InstallPackages", &(flags, ids)).await?,
        Call::Remove(flags, ids, allow_deps, autoremove) => tx.call_method("RemovePackages", &(flags, ids, allow_deps, autoremove)).await?,
        Call::RefreshCache => tx.call_method("RefreshCache", &(false,)).await?,
    };
    let mut out = Outcome::default();
    let (mut percentage, mut status) = (None::<u32>, 0u32);
    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let header = msg.header();
        let Some(member) = header.member() else { continue };
        match member.as_str() {
            "Package" => {
                if let Ok((info, id, _summary)) = msg.body().deserialize::<(u32, String, String)>() {
                    out.packages.push((info, id));
                }
            }
            "ErrorCode" => {
                if let Ok((code, details)) = msg.body().deserialize::<(u32, String)>() {
                    out.error = Some((code, details));
                }
            }
            "PropertiesChanged" => {
                let Ok((_, changed, _)) = msg.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>() else { continue };
                let get = |key: &str| changed.get(key).and_then(|v| v.downcast_ref::<u32>().ok());
                let before = (percentage, status);
                if let Some(p) = get("Percentage") {
                    percentage = (p <= 100).then_some(p);
                }
                if let Some(s) = get("Status") {
                    status = s;
                }
                if (percentage, status) != before {
                    progress(percentage.map(|p| p as f32 / 100.0), status);
                }
            }
            "Finished" => {
                if let Ok((exit, _runtime)) = msg.body().deserialize::<(u32, u32)>() {
                    out.exit = exit;
                }
                break;
            }
            "Destroy" => break,
            _ => {}
        }
    }
    Ok(out)
}

/// What a status means to the user; None keeps the previous text
fn status_text(op: Op, status: u32) -> Option<&'static str> {
    Some(match status {
        1 | 2 => "Подготовка…",
        31 => "Ожидание пароля администратора…",
        30 => "Ожидание другой установки или обновления…",
        7 | 20 | 21 => "Обновление списка программ…",
        13 | 4 | 5 | 27 => "Проверка зависимостей…",
        8 => "Загрузка…",
        14 => "Проверка подписей…",
        9 | 10 | 3 | 15 | 16 if op == Op::Install => "Установка…",
        6 | 3 | 15 | 16 => "Удаление…",
        11 => "Завершение…",
        _ => return None,
    })
}

/// The final event of a transaction that did not succeed
fn failure(out: &Outcome) -> Option<Event> {
    if out.exit == EXIT_SUCCESS && out.error.is_none() {
        return None;
    }
    let Some((code, details)) = &out.error else {
        return Some(if out.exit == EXIT_CANCELLED { Event::Cancelled } else { Event::Failed("Операция не удалась".into()) });
    };
    Some(match *code {
        ERROR_NOT_AUTHORIZED | ERROR_TRANSACTION_CANCELLED => Event::Cancelled,
        ERROR_NO_NETWORK | ERROR_PACKAGE_DOWNLOAD_FAILED | ERROR_REPO_NOT_AVAILABLE | ERROR_NO_MORE_MIRRORS => {
            Event::Failed("Не удалось скачать пакеты. Проверьте подключение к интернету".into())
        }
        ERROR_CANNOT_GET_LOCK => Event::Failed("Сейчас идёт другая установка или обновление. Повторите позже".into()),
        ERROR_NO_SPACE => Event::Failed("Недостаточно места на диске".into()),
        ERROR_CANNOT_REMOVE_SYSTEM_PACKAGE => Event::Failed("Это часть системы, её нельзя удалить".into()),
        ERROR_DEP_RESOLUTION_FAILED => Event::Failed(format!("Не удаётся подобрать зависимости: {}", first_lines(details))),
        _ if details.trim().is_empty() => Event::Failed("Операция не удалась".into()),
        _ => Event::Failed(first_lines(details)),
    })
}

fn first_lines(text: &str) -> String {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).take(3).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_and_removals() {
        let ok = Outcome { exit: EXIT_SUCCESS, ..Default::default() };
        assert_eq!(failure(&ok), None);
        let denied = Outcome { exit: 2, error: Some((ERROR_NOT_AUTHORIZED, "Failed to obtain authentication.".into())), ..Default::default() };
        assert_eq!(failure(&denied), Some(Event::Cancelled));
        let lock = Outcome { exit: 2, error: Some((ERROR_CANNOT_GET_LOCK, "x".into())), ..Default::default() };
        assert!(matches!(failure(&lock), Some(Event::Failed(t)) if t.contains("другая установка")));
        let packages = vec![
            (INFO_REMOVING, "gimp;3.0;amd64;installed".to_string()),
            (INFO_REMOVING, "gimp-plugin-registry;1;amd64;installed".to_string()),
            (1, "libgimp;3.0;amd64;installed".to_string()),
        ];
        assert_eq!(also_removed(&packages, "gimp"), ["gimp-plugin-registry"]);
        assert_eq!(name_of("vlc;3.0.21;amd64;ubuntu-resolute-universe"), "vlc");
    }

    /// Talks to the real PackageKit without changing anything (resolve + simulated removal):
    /// FEATHER_STORE_PK=<installed package> cargo test -p feather-store -- --ignored pk_
    #[test]
    #[ignore]
    fn pk_simulated_removal() {
        let Ok(package) = std::env::var("FEATHER_STORE_PK") else { return };
        futures_lite::future::block_on(async {
            let conn = Connection::system().await.unwrap();
            let id = resolve(&conn, Op::Remove, &package).await.unwrap();
            eprintln!("installed id: {id:?}");
            let available = resolve(&conn, Op::Install, "hivelytracker").await.unwrap();
            eprintln!("installable hivelytracker: {available:?}");
            let simulated = transaction(&conn, Call::Remove(FLAG_SIMULATE, vec![id.unwrap()], true, false), &|p, s| eprintln!("progress {p:?} status {s}")).await.unwrap();
            eprintln!("exit {} error {:?}", simulated.exit, simulated.error);
            eprintln!("also removed: {:?}", also_removed(&simulated.packages, &package));
        });
    }
}
