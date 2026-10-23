//! polkit authentication agent. D-Bus runs on its own thread; PAM runs in root's
//! `polkit-agent-helper-1` (socket-activated, or the setuid helper on older polkit).
//! The compositor only draws the dialog and forwards what the user typed.
use smithay::reexports::calloop::{channel, LoopHandle};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    sync::{mpsc, Arc, Mutex},
};
use zbus::{message::Header, zvariant::OwnedValue};
use zeroize::Zeroizing;

const OBJECT: &str = "/org/feather/PolicyKit1/AuthenticationAgent";
const POLKITD: &str = "org.freedesktop.PolicyKit1";
const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";
const HELPER: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";
/// Concurrent requests polkitd may hold open against this agent.
const MAX_PENDING: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub uid: u32,
    pub name: String,
    pub real_name: String,
}
impl User {
    pub fn display(&self) -> &str {
        if self.real_name.is_empty() {
            &self.name
        } else {
            &self.real_name
        }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub cookie: String,
    pub action_id: String,
    pub message: String,
    pub details: Vec<(String, String)>,
    pub users: Vec<User>,
    pub default_user: usize,
}

#[derive(Debug)]
pub enum Event {
    /// Registration failed (another agent owns the session, no polkitd): use the fallback.
    Unavailable(String),
    Begin(Request),
    /// polkitd withdrew the request.
    Cancel(String),
    /// A further PAM question after the password (one-time code and the like).
    Prompt { cookie: String, text: String, echo: bool },
    Info { cookie: String, text: String, error: bool },
    Done { cookie: String, ok: bool },
}

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Cancelled(String),
    Failed(String),
}

type Waiters = Arc<Mutex<HashMap<String, async_channel::Sender<bool>>>>;

#[derive(Clone)]
pub struct Agent {
    waiters: Waiters,
    events: channel::Sender<Event>,
}

impl Agent {
    /// `replace_external`: stop lxqt/KDE agents of *this* login session first
    /// (agents of other sessions, e.g. another desktop on another VT, are left alone).
    pub fn start(
        handle: &LoopHandle<'static, crate::FeatherCompositor>,
        replace_external: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (events, source) = channel::channel();
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                state.polkit_event(event);
            }
        })?;
        let agent = Self {
            waiters: Arc::default(),
            events,
        };
        let iface = Interface {
            waiters: agent.waiters.clone(),
            events: agent.events.clone(),
        };
        let events = agent.events.clone();
        std::thread::Builder::new()
            .name("feather-polkit".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                if replace_external && stop_session_agents() {
                    // Let polkitd notice the old agent's bus name vanish.
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                futures_lite::future::block_on(async {
                    if let Err(e) = serve(iface).await {
                        let _ = events.send(Event::Unavailable(e.to_string()));
                    }
                })
            })?;
        Ok(agent)
    }

    /// Without D-Bus: requests are injected by nested checks (`FEATHER_DEBUG_POLKIT`).
    pub fn detached(handle: &LoopHandle<'static, crate::FeatherCompositor>) -> Result<Self, Box<dyn std::error::Error>> {
        let (events, source) = channel::channel();
        handle.insert_source(source, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                state.polkit_event(event);
            }
        })?;
        Ok(Self { waiters: Arc::default(), events })
    }
    pub fn inject(&self, request: Request) {
        let (tx, _rx) = async_channel::bounded(1);
        self.waiters.lock().unwrap().insert(request.cookie.clone(), tx);
        let _ = self.events.send(Event::Begin(request));
    }

    /// Completes `BeginAuthentication`: `true` after the helper reported success
    /// (it has already told polkitd), `false` when the user cancelled.
    pub fn finish(&self, cookie: &str, ok: bool) {
        if let Some(tx) = self.waiters.lock().unwrap().remove(cookie) {
            let _ = tx.try_send(ok);
        }
    }

    /// One PAM conversation in a worker thread. Dropping the session aborts it.
    pub fn authenticate(&self, cookie: String, user: String, password: Zeroizing<String>) -> Session {
        let (responses, rx) = mpsc::channel::<Zeroizing<String>>();
        let abort: Arc<Mutex<Option<Abort>>> = Arc::default();
        let agent = self.clone();
        let slot = abort.clone();
        let spawned = std::thread::Builder::new()
            .name("feather-polkit-pam".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let ok = converse(&helper_socket(), &agent.events, &cookie, &user, password, &rx, &slot).unwrap_or_else(|e| {
                    tracing::warn!("polkit helper: {e}");
                    false
                });
                if ok {
                    agent.finish(&cookie, true);
                }
                let _ = agent.events.send(Event::Done { cookie, ok });
            });
        if spawned.is_err() {
            tracing::warn!("polkit: cannot start helper thread");
        }
        Session { responses, abort }
    }
}

enum Abort {
    Socket(UnixStream),
    Child(std::process::Child),
}

pub struct Session {
    responses: mpsc::Sender<Zeroizing<String>>,
    abort: Arc<Mutex<Option<Abort>>>,
}
impl Session {
    pub fn respond(&self, answer: Zeroizing<String>) {
        let _ = self.responses.send(answer);
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        match self.abort.lock().unwrap().take() {
            Some(Abort::Socket(s)) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            Some(Abort::Child(mut c)) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            None => {}
        }
    }
}

/// SIGTERM to external polkit agents that belong to our XDG session. True if any.
fn stop_session_agents() -> bool {
    let Ok(session) = std::env::var("XDG_SESSION_ID") else { return false };
    let me = unsafe { libc::getuid() };
    let mut stopped = false;
    let Ok(dir) = std::fs::read_dir("/proc") else { return false };
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        let base = entry.path();
        let comm = std::fs::read_to_string(base.join("comm")).unwrap_or_default();
        if !matches!(comm.trim(), "lxqt-policykit-agent" | "lxqt-policykit-" | "polkit-kde-authentication-agent-1" | "polkit-kde-auth") {
            continue;
        }
        let owned = std::fs::metadata(&base).map(|m| std::os::unix::fs::MetadataExt::uid(&m) == me).unwrap_or(false);
        let environ = std::fs::read(base.join("environ")).unwrap_or_default();
        let ours = environ.split(|b| *b == 0).any(|v| v == format!("XDG_SESSION_ID={session}").as_bytes());
        if owned && ours {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            stopped = true;
        }
    }
    stopped
}

fn helper_socket() -> String {
    // Tests point this at a fake helper; production always uses the system socket.
    std::env::var("FEATHER_POLKIT_HELPER_SOCKET").unwrap_or_else(|_| HELPER_SOCKET.into())
}

/// The helper protocol of libpolkit-agent: `user\n`, `cookie\n`, then prompt lines
/// answered one line each, ending with SUCCESS or FAILURE.
fn converse(
    path: &str,
    events: &channel::Sender<Event>,
    cookie: &str,
    user: &str,
    password: Zeroizing<String>,
    responses: &mpsc::Receiver<Zeroizing<String>>,
    abort: &Mutex<Option<Abort>>,
) -> std::io::Result<bool> {
    if user.contains(['\n', '\0']) || cookie.contains(['\n', '\0']) {
        return Ok(false);
    }
    let (mut reader, mut writer): (Box<dyn BufRead + Send>, Box<dyn Write + Send>) =
        if std::path::Path::new(path).exists() {
            let stream = UnixStream::connect(path)?;
            *abort.lock().unwrap() = Some(Abort::Socket(stream.try_clone()?));
            let mut writer = stream.try_clone()?;
            writer.write_all(format!("{user}\n").as_bytes())?;
            (Box::new(BufReader::new(stream)), Box::new(writer))
        } else {
            let mut child = std::process::Command::new(HELPER)
                .arg(user)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            let (stdin, stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
            *abort.lock().unwrap() = Some(Abort::Child(child));
            (Box::new(BufReader::new(stdout)), Box::new(stdin))
        };
    writer.write_all(format!("{cookie}\n").as_bytes())?;
    writer.flush()?;
    let mut password = Some(password);
    loop {
        let mut line = Zeroizing::new(String::new());
        if (&mut reader).take(4096).read_line(&mut line)? == 0 {
            return Ok(false);
        }
        let text = unescape(line.trim_end_matches('\n'));
        let prompt = |prefix: &str| text.strip_prefix(prefix).map(str::to_owned);
        if let Some((text, echo)) = prompt("PAM_PROMPT_ECHO_OFF ")
            .map(|t| (t, false))
            .or_else(|| prompt("PAM_PROMPT_ECHO_ON ").map(|t| (t, true)))
        {
            let mut answer = match password.take().filter(|_| !echo) {
                Some(p) => p,
                None => {
                    let _ = events.send(Event::Prompt { cookie: cookie.into(), text, echo });
                    match responses.recv() {
                        Ok(answer) => answer,
                        Err(_) => return Ok(false),
                    }
                }
            };
            answer.retain(|c| c != '\n' && c != '\0');
            answer.push('\n');
            writer.write_all(answer.as_bytes())?;
            writer.flush()?;
        } else if let Some(text) = prompt("PAM_ERROR_MSG ") {
            let _ = events.send(Event::Info { cookie: cookie.into(), text, error: true });
        } else if let Some(text) = prompt("PAM_TEXT_INFO ") {
            let _ = events.send(Event::Info { cookie: cookie.into(), text, error: false });
        } else {
            return Ok(text == "SUCCESS");
        }
    }
}

/// g_strcompress: the helper escapes PAM text C-style.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') | Some('f') | Some('v') => {}
            Some(d @ '0'..='7') => {
                let mut v = d.to_digit(8).unwrap();
                for _ in 0..2 {
                    match chars.clone().next().and_then(|c| c.to_digit(8)) {
                        Some(n) => {
                            v = v * 8 + n;
                            chars.next();
                        }
                        None => break,
                    }
                }
                if let Some(c) = char::from_u32(v).filter(|c| !c.is_control() || *c == '\n') {
                    out.push(c);
                }
            }
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

struct Interface {
    waiters: Waiters,
    events: channel::Sender<Event>,
}

impl Interface {
    /// Only polkitd may open dialogs: any other system-bus peer is refused.
    async fn check(&self, header: &Header<'_>, conn: &zbus::Connection) -> Result<(), AgentError> {
        let owner = zbus::fdo::DBusProxy::new(conn)
            .await?
            .get_name_owner(POLKITD.try_into().map_err(zbus::Error::from)?)
            .await
            .map_err(|e| AgentError::Failed(e.to_string()))?;
        if header.sender().map(|s| s.as_str()) == Some(owner.as_str()) {
            Ok(())
        } else {
            Err(AgentError::Failed("only polkitd may call this agent".into()))
        }
    }
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Interface {
    #[allow(clippy::too_many_arguments)]
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), AgentError> {
        let _ = icon_name;
        self.check(&header, conn).await?;
        let users = users(&identities);
        if users.is_empty() || cookie.is_empty() || cookie.len() > 512 {
            return Err(AgentError::Failed("no usable identity".into()));
        }
        let (tx, rx) = async_channel::bounded(1);
        {
            let mut waiters = self.waiters.lock().unwrap();
            if waiters.len() >= MAX_PENDING || waiters.contains_key(&cookie) {
                return Err(AgentError::Failed("too many requests".into()));
            }
            waiters.insert(cookie.clone(), tx);
        }
        let me = unsafe { libc::getuid() };
        let mut details: Vec<_> = details
            .into_iter()
            .filter(|(k, v)| k.len() <= 128 && v.len() <= 2048)
            .take(16)
            .collect();
        details.sort();
        let request = Request {
            cookie: cookie.clone(),
            action_id: clip(action_id, 256),
            message: clip(message, 1024),
            details,
            default_user: users.iter().position(|u| u.uid == me).unwrap_or(0),
            users,
        };
        let _ = self.events.send(Event::Begin(request));
        match rx.recv().await {
            Ok(true) => Ok(()),
            _ => {
                self.waiters.lock().unwrap().remove(&cookie);
                Err(AgentError::Cancelled("Отменено пользователем".into()))
            }
        }
    }

    async fn cancel_authentication(
        &self,
        cookie: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), AgentError> {
        self.check(&header, conn).await?;
        if let Some(tx) = self.waiters.lock().unwrap().remove(&cookie) {
            let _ = tx.try_send(false);
            let _ = self.events.send(Event::Cancel(cookie));
        }
        Ok(())
    }
}

async fn serve(iface: Interface) -> zbus::Result<()> {
    let conn = zbus::connection::Builder::system()?
        .serve_at(OBJECT, iface)?
        .build()
        .await?;
    let locale = std::env::var("LANG").unwrap_or_else(|_| "C".into());
    conn.call_method(
        Some(POLKITD),
        "/org/freedesktop/PolicyKit1/Authority",
        Some("org.freedesktop.PolicyKit1.Authority"),
        "RegisterAuthenticationAgent",
        &(subject(), locale, OBJECT),
    )
    .await?;
    tracing::info!("polkit: собственный агент зарегистрирован");
    std::future::pending::<()>().await;
    Ok(())
}

/// The login session we serve (polkit binds one agent per session).
fn subject() -> (&'static str, HashMap<&'static str, zbus::zvariant::Value<'static>>) {
    if let Ok(id) = std::env::var("XDG_SESSION_ID") {
        if !id.is_empty() {
            return ("unix-session", HashMap::from([("session-id", id.into())]));
        }
    }
    let start = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            let rest = s.rsplit_once(')')?.1.to_owned();
            rest.split_whitespace().nth(19)?.parse::<u64>().ok()
        })
        .unwrap_or(0);
    (
        "unix-process",
        HashMap::from([
            ("pid", std::process::id().into()),
            ("start-time", start.into()),
        ]),
    )
}

fn clip(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

fn users(identities: &[(String, HashMap<String, OwnedValue>)]) -> Vec<User> {
    let mut out: Vec<User> = Vec::new();
    let mut add = |u: Option<User>| {
        if let Some(u) = u {
            if !out.iter().any(|o| o.uid == u.uid) && out.len() < 32 {
                out.push(u);
            }
        }
    };
    for (kind, data) in identities.iter().take(64) {
        let number = |key: &str| data.get(key).and_then(|v| u32::try_from(&**v).ok());
        match kind.as_str() {
            "unix-user" => add(number("uid").and_then(user_by_uid)),
            "unix-group" => {
                for name in number("gid").map(group_members).unwrap_or_default() {
                    add(user_by_name(&name));
                }
            }
            _ => {}
        }
    }
    out
}

fn passwd(pw: &libc::passwd) -> User {
    let cstr = |p: *const libc::c_char| {
        if p.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    let gecos = cstr(pw.pw_gecos);
    User {
        uid: pw.pw_uid,
        name: cstr(pw.pw_name),
        real_name: gecos.split(',').next().unwrap_or("").trim().to_owned(),
    }
}

fn user_by_uid(uid: u32) -> Option<User> {
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut result = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then(|| passwd(&pw))
}

fn user_by_name(name: &str) -> Option<User> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut result = std::ptr::null_mut();
    let rc = unsafe { libc::getpwnam_r(name.as_ptr(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then(|| passwd(&pw))
}

fn group_members(gid: u32) -> Vec<String> {
    let mut gr: libc::group = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 65536];
    let mut result = std::ptr::null_mut();
    let rc = unsafe { libc::getgrgid_r(gid, &mut gr, buf.as_mut_ptr(), buf.len(), &mut result) };
    let mut out = Vec::new();
    if rc == 0 && !result.is_null() && !gr.gr_mem.is_null() {
        let mut p = gr.gr_mem;
        unsafe {
            while !(*p).is_null() && out.len() < 64 {
                out.push(std::ffi::CStr::from_ptr(*p).to_string_lossy().into_owned());
                p = p.add(1);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Fake socket helper speaking polkit's line protocol.
    pub(crate) fn fake_helper(path: &std::path::Path, password: &'static str, otp: Option<&'static str>) {
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut w = stream;
                let mut line = String::new();
                let mut read = |r: &mut BufReader<UnixStream>| {
                    line.clear();
                    r.read_line(&mut line).ok();
                    line.trim_end().to_owned()
                };
                let (user, cookie) = (read(&mut reader), read(&mut reader));
                let _ = writeln!(w, "PAM_TEXT_INFO user {user} cookie {cookie}");
                let _ = writeln!(w, "PAM_PROMPT_ECHO_OFF \\320\\237\\320\\260\\321\\200\\320\\276\\320\\273\\321\\214: ");
                let mut ok = read(&mut reader) == password;
                if let (true, Some(code)) = (ok, otp) {
                    let _ = writeln!(w, "PAM_PROMPT_ECHO_ON Код: ");
                    ok = read(&mut reader) == code;
                }
                if !ok {
                    let _ = writeln!(w, "PAM_ERROR_MSG Неверный пароль");
                }
                let _ = writeln!(w, "{}", if ok { "SUCCESS" } else { "FAILURE" });
            }
        });
    }
    #[test]
    fn conversation_with_socket_helper() {
        let dir = std::env::temp_dir().join(format!("feather-polkit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("helper.socket");
        fake_helper(&path, "секрет", Some("123456"));
        let path = path.to_str().unwrap().to_owned();
        let (events, rx_events) = channel::channel();
        let abort = Mutex::new(None);
        let run = |password: &str, answers: Vec<&str>| {
            let (tx, rx) = mpsc::channel();
            for a in answers {
                tx.send(Zeroizing::new(a.to_owned())).unwrap();
            }
            drop(tx); // as when the dialog closes: no further answers
            converse(&path, &events, "c1", "zumka", Zeroizing::new(password.into()), &rx, &abort).unwrap()
        };
        assert!(run("секрет", vec!["123456"]));
        assert!(!run("секрет", vec!["000000"]));
        assert!(!run("неверно", vec![]));
        // Second prompt without an answer (dialog closed): the conversation ends, no hang.
        assert!(!run("секрет", vec![]));
        let got: Vec<_> = std::iter::from_fn(|| rx_events.try_recv().ok()).collect();
        assert!(got.iter().any(|e| matches!(e, Event::Info { text, .. } if text == "user zumka cookie c1")));
        assert!(got.iter().any(|e| matches!(e, Event::Prompt { text, echo: true, .. } if text == "Код: ")));
        assert!(got.iter().any(|e| matches!(e, Event::Info { error: true, .. })));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn helper_text_is_unescaped_like_g_strcompress() {
        assert_eq!(unescape(r"Пароль: "), "Пароль: ");
        assert_eq!(unescape(r"a\nb\\c\101"), "a\nb\\cA");
    }
    #[test]
    fn identities_resolve_current_user_and_skip_unknown_kinds() {
        let me = unsafe { libc::getuid() };
        let ids = vec![
            ("unix-user".to_string(), HashMap::from([("uid".to_string(), OwnedValue::from(me))])),
            ("unix-user".to_string(), HashMap::from([("uid".to_string(), OwnedValue::from(me))])),
            ("bogus".to_string(), HashMap::new()),
        ];
        let users = users(&ids);
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].uid, me);
        assert!(!users[0].name.is_empty());
    }
}
