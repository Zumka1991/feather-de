//! Демонстрация экрана для OBS, браузеров и звонков: поток PipeWire с кадрами монитора.
//!
//! PipeWire живёт в своём потоке `feather-pipewire` со своим циклом: цикл событий композитора
//! никогда его не ждёт. Композитор шлёт готовые кадры RGBA (`ToPw::Frame`), поток кладёт
//! последний кадр в буфер PipeWire и запускает цикл графа (`trigger_process`). Обратно идут
//! номер узла и состояние потока (`FromPw`) через calloop-канал.
//!
//! Кадры снимаются только когда монитор действительно перерисовался, не чаще `fps` в секунду:
//! в простое нет ни снятия, ни копирования (0 % CPU). Поток стартует при первой демонстрации
//! и дальше спит в своём цикле.
use pipewire as pw;
use pw::spa;
use smithay::reexports::calloop::channel;
use smithay::{
    backend::renderer::{gles::{GlesMapping, GlesTexture}, ExportMem},
    reexports::calloop::timer::{TimeoutAction, Timer},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::{api::{Reply, Responder}, FeatherCompositor};

/// Наибольшая частота кадров, которую предлагаем потребителю
pub const MAX_FPS: u32 = 60;
/// Для 4K CPU readback (33 МБ/кадр) ограничиваем частоту 30 к/с, чтобы главный поток
/// композитора оставался в бюджете < 1 мс и не задерживал ввод libinput.
const UHD_READBACK_FPS: u32 = 30;

fn cast_fps(width: i32, height: i32) -> u32 {
    if (width as i64) * (height as i64) > 2560 * 1440 {
        UHD_READBACK_FPS
    } else {
        MAX_FPS
    }
}

/// Zero-copy GPU PBO readback frame shared across casts of the same monitor.
/// Dropping the last `Arc<CastFrame>` drops `GlesMapping`, which in Smithay queues the
/// GL buffer unmap for the next main-thread renderer cleanup (0 bytes of `RssAnon` heap).
pub struct CastFrame {
    ptr: *const u8,
    len: usize,
    _mapping: GlesMapping,
}

// SAFETY: `ptr` points into a completed GL pixel-pack buffer mapping that remains immutable
// and mapped until `_mapping` is dropped; PipeWire workers only read `&[u8]`.
unsafe impl Send for CastFrame {}
unsafe impl Sync for CastFrame {}

impl CastFrame {
    #[inline]
    fn bytes(&self) -> &[u8] {
        // SAFETY: valid for `self.len` bytes for the entire lifetime of `self._mapping`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

pub enum ToPw {
    Start { id: u64, width: u32, height: u32, in_flight: Arc<AtomicBool> },
    /// Плотно упакованные строки RGBA сверху вниз прямо из GPU PBO без копии в главном потоке
    Frame { id: u64, frame: Arc<CastFrame> },
    Stop(u64),
}

#[derive(Debug)]
pub enum FromPw {
    /// Узел создан: его номер получает портал
    Node { id: u64, node: u32 },
    /// Потребитель подключился (`true`) или приостановил поток
    Streaming { id: u64, on: bool },
    /// Кадр скопирован в PipeWire MemFd: композитор может снимать следующий, если экран менялся
    FrameDone { id: u64 },
    Failed { id: u64, reason: String },
    BusDown(String),
}

struct Shared {
    id: u64,
    width: u32,
    height: u32,
    events: channel::Sender<FromPw>,
    in_flight: Arc<AtomicBool>,
    node_sent: bool,
    /// Последний кадр, ещё не отданный в буфер
    frame: Option<Arc<CastFrame>>,
}

struct Cast {
    stream: pw::stream::StreamRc,
    shared: Rc<RefCell<Shared>>,
    _listener: pw::stream::StreamListener<Rc<RefCell<Shared>>>,
}

/// Запускает поток PipeWire без ожидания в цикле событий композитора
pub fn spawn(events: channel::Sender<FromPw>) -> Option<pw::channel::Sender<ToPw>> {
    let (tx, rx) = pw::channel::channel::<ToPw>();
    let spawned = std::thread::Builder::new()
        .name("feather-pipewire".into())
        .stack_size(1024 * 1024)
        .spawn(move || {
            if let Err(e) = run(rx, events.clone()) {
                tracing::warn!("PipeWire: {e}");
                let _ = events.send(FromPw::BusDown(e.to_string()));
            }
        });
    spawned.ok().map(|_| tx)
}

fn run(
    rx: pw::channel::Receiver<ToPw>,
    events: channel::Sender<FromPw>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let casts: Rc<RefCell<HashMap<u64, Cast>>> = Rc::new(RefCell::new(HashMap::new()));
    let casts_in = casts.clone();
    let _receiver = rx.attach(mainloop.loop_(), move |msg| match msg {
        ToPw::Start { id, width, height, in_flight } => {
            match start(&core, id, width, height, in_flight, events.clone()) {
                Ok(cast) => {
                    casts_in.borrow_mut().insert(id, cast);
                }
                Err(e) => {
                    let _ = events.send(FromPw::Failed { id, reason: e.to_string() });
                }
            }
        }
        ToPw::Frame { id, frame } => {
            let casts = casts_in.borrow();
            let Some(cast) = casts.get(&id) else { return };
            if cast.stream.state() != pw::stream::StreamState::Streaming {
                cast.shared.borrow().in_flight.store(false, Ordering::Release);
                return;
            }
            cast.shared.borrow_mut().frame = Some(frame);
            // We drive the graph: one cycle per new frame. Under another driver the
            // frame waits for its next process call.
            if cast.stream.is_driving() {
                let _ = cast.stream.trigger_process();
            }
        }
        ToPw::Stop(id) => {
            if let Some(cast) = casts_in.borrow_mut().remove(&id) {
                let mut s = cast.shared.borrow_mut();
                s.frame = None;
                s.in_flight.store(false, Ordering::Release);
                drop(s);
                let _ = cast.stream.disconnect();
            }
        }
    });
    mainloop.run();
    drop(casts);
    Ok(())
}

fn start(
    core: &pw::core::CoreRc,
    id: u64,
    width: u32,
    height: u32,
    in_flight: Arc<AtomicBool>,
    events: channel::Sender<FromPw>,
) -> Result<Cast, pw::Error> {
    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "feather-screen-cast",
        pw::properties::properties! {
            *pw::keys::MEDIA_CLASS => "Video/Source",
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => "feather-screen-cast",
            *pw::keys::NODE_DESCRIPTION => "Экран Пёрышка",
        },
    )?;
    let shared = Rc::new(RefCell::new(Shared {
        id,
        width,
        height,
        events,
        in_flight,
        node_sent: false,
        frame: None,
    }));
    let listener = stream
        .add_local_listener_with_user_data(shared.clone())
        .state_changed(|stream, shared, _old, new| {
            let mut s = shared.borrow_mut();
            match new {
                pw::stream::StreamState::Paused => {
                    s.frame = None;
                    s.in_flight.store(false, Ordering::Release);
                    if !s.node_sent {
                        s.node_sent = true;
                        let _ = s.events.send(FromPw::Node { id: s.id, node: stream.node_id() });
                    } else {
                        let _ = s.events.send(FromPw::Streaming { id: s.id, on: false });
                    }
                }
                pw::stream::StreamState::Streaming => {
                    let _ = s.events.send(FromPw::Streaming { id: s.id, on: true });
                }
                pw::stream::StreamState::Error(reason) => {
                    s.frame = None;
                    s.in_flight.store(false, Ordering::Release);
                    let _ = s.events.send(FromPw::Failed { id: s.id, reason });
                }
                _ => {}
            }
        })
        .param_changed(|stream, shared, param_id, param| {
            if param_id != spa::param::ParamType::Format.as_raw() || param.is_none() {
                return;
            }
            let (w, h) = {
                let s = shared.borrow();
                (s.width, s.height)
            };
            let Some(buffers) = serialize(buffers_param(w, h)) else { return };
            let Some(b) = spa::pod::Pod::from_bytes(&buffers) else { return };
            let _ = stream.update_params(&mut [b]);
        })
        .process(|stream, shared| {
            let mut s = shared.borrow_mut();
            let Some(frame) = s.frame.take() else { return };
            let Some(mut buffer) = stream.dequeue_buffer() else {
                // Every buffer is still with the consumer: keep the frame for the next cycle
                s.frame = Some(frame);
                return;
            };
            let stride = s.width * 4;
            let size = stride * s.height;
            let src = frame.bytes();
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                s.in_flight.store(false, Ordering::Release);
                let _ = s.events.send(FromPw::FrameDone { id: s.id });
                return;
            };
            let written = match data.data() {
                Some(dst) => {
                    let n = dst.len().min(src.len()).min(size as usize);
                    dst[..n].copy_from_slice(&src[..n]);
                    n as u32
                }
                None => 0,
            };
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = written;
            drop(frame);
            s.in_flight.store(false, Ordering::Release);
            let _ = s.events.send(FromPw::FrameDone { id: s.id });
            // The buffer is queued back to the stream when dropped
        })
        .register()?;

    let format = serialize(format_param(width, height)).ok_or(pw::Error::CreationFailed)?;
    let pod = spa::pod::Pod::from_bytes(&format).ok_or(pw::Error::CreationFailed)?;
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::DRIVER | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [pod],
    )?;
    Ok(Cast { stream, shared, _listener: listener })
}

fn serialize(object: spa::pod::Object) -> Option<Vec<u8>> {
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .ok()
    .map(|(cursor, _)| cursor.into_inner())
}

fn choice_int(default: i32, min: i32, max: i32) -> spa::pod::Value {
    spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
        spa::utils::ChoiceFlags::empty(),
        spa::utils::ChoiceEnum::Range { default, min, max },
    )))
}

/// Один размер (монитор), RGBA/RGBx, переменная частота до `MAX_FPS`
fn format_param(width: u32, height: u32) -> spa::pod::Object {
    use spa::param::format::{FormatProperties as F, MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;
    use spa::pod::{ChoiceValue, Property, Value};
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle};
    spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(F::MediaType.as_raw(), Value::Id(Id(MediaType::Video.as_raw()))),
            Property::new(F::MediaSubtype.as_raw(), Value::Id(Id(MediaSubtype::Raw.as_raw()))),
            Property::new(
                F::VideoFormat.as_raw(),
                Value::Choice(ChoiceValue::Id(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: Id(VideoFormat::RGBA.as_raw()),
                        alternatives: vec![Id(VideoFormat::RGBA.as_raw()), Id(VideoFormat::RGBx.as_raw())],
                    },
                ))),
            ),
            Property::new(F::VideoSize.as_raw(), Value::Rectangle(Rectangle { width, height })),
            Property::new(F::VideoFramerate.as_raw(), Value::Fraction(Fraction { num: 0, denom: 1 })),
            Property::new(
                F::VideoMaxFramerate.as_raw(),
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Fraction { num: MAX_FPS, denom: 1 },
                        min: Fraction { num: 1, denom: 1 },
                        max: Fraction { num: MAX_FPS, denom: 1 },
                    },
                ))),
            ),
        ],
    }
}

/// Буферы в памяти (memfd): одна плоскость, строки без выравнивания.
/// `SPA_PARAM_BUFFERS_dataType` обязан быть `SPA_CHOICE_Flags`, иначе PipeWire
/// сравнивает битовую маску на точное равенство и отвергает OBS (`Int 14` = MemFd|MemPtr|DmaBuf)
/// с `error alloc buffers: Invalid argument (-22)`.
fn buffers_param(width: u32, height: u32) -> spa::pod::Object {
    use spa::pod::{ChoiceValue, Property, Value};
    use spa::sys;
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags};
    let stride = (width * 4) as i32;
    let mem_types = ((1 << sys::SPA_DATA_MemFd) | (1 << sys::SPA_DATA_MemPtr)) as i32;
    spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
        id: spa::param::ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(sys::SPA_PARAM_BUFFERS_buffers, choice_int(2, 1, 3)),
            Property::new(sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            Property::new(sys::SPA_PARAM_BUFFERS_size, Value::Int(stride * height as i32)),
            Property::new(sys::SPA_PARAM_BUFFERS_stride, Value::Int(stride)),
            Property::new(
                sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: mem_types,
                        flags: Vec::new(),
                    },
                ))),
            ),
        ],
    }
}

/// Сколько ждём, пока видеокарта асинхронно допишет 4K-кадр в PBO, прежде чем вызывать map_texture
/// (10 мс избавляет главный поток от синхронного ожидания GPU и задержек libinput)
const READBACK_DELAY: Duration = Duration::from_millis(10);
/// Не больше потоков одновременно
const MAX_CASTS: usize = 4;

struct ActiveCast {
    id: u64,
    monitor: String,
    cursor: bool,
    /// Физический размер кадра
    size: (i32, i32),
    rect: (i32, i32, i32, i32),
    /// Портал ждёт номер узла
    reply: Option<Responder>,
    streaming: bool,
    /// Монитор перерисовался после последнего снятого кадра
    dirty: bool,
    /// Предыдущий кадр ещё копируется в PipeWire: не запускаем новый PBO readback
    in_flight: Arc<AtomicBool>,
    offscreen: Option<(GlesTexture, smithay::backend::renderer::damage::OutputDamageTracker, (i32, i32))>,
    last: Option<Instant>,
}

struct PendingCapture {
    size: (i32, i32),
    cast_ids: Vec<u64>,
    mapping: GlesMapping,
}

#[derive(Default)]
pub struct ScreenCasts {
    tx: Option<pw::channel::Sender<ToPw>>,
    casts: Vec<ActiveCast>,
    pending: Vec<PendingCapture>,
    next_id: u64,
    flush_armed: bool,
    due_armed: Option<Instant>,
}

impl ScreenCasts {
    pub fn is_empty(&self) -> bool {
        self.casts.is_empty()
    }

    pub fn is_streaming(&self) -> bool {
        self.casts.iter().any(|c| c.streaming)
    }

    fn send(&self, msg: ToPw) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg);
        }
    }
}

impl FeatherCompositor {
    pub fn cast_start(&mut self, cursor: bool, monitor: &str, reply: Responder) {
        use feather_api::Error;
        if self.casts.casts.len() >= MAX_CASTS {
            return reply.send(Err(Error::Unavailable("Слишком много демонстраций экрана".into())));
        }
        let info = if monitor.is_empty() {
            let (x, y) = (self.pointer_location.x, self.pointer_location.y);
            self.monitors
                .iter()
                .find(|m| m.info.rect.contains(x, y))
                .or_else(|| self.primary_monitor())
                .map(|m| m.info.clone())
        } else {
            self.monitors.iter().find(|m| m.info.name == monitor).map(|m| m.info.clone())
        };
        let Some(info) = info else {
            return reply.send(Err(Error::NotFound(format!("Нет монитора «{monitor}»"))));
        };
        if self.casts.tx.is_none() {
            let (events, source) = channel::channel::<FromPw>();
            let inserted = self.loop_handle.insert_source(source, |event, _, state| {
                if let channel::Event::Msg(msg) = event {
                    state.cast_event(msg);
                }
            });
            if inserted.is_err() {
                return reply.send(Err(Error::Failed("Не удалось подключить PipeWire".into())));
            }
            self.casts.tx = spawn(events);
            if self.casts.tx.is_none() {
                return reply.send(Err(Error::Unavailable("PipeWire недоступен".into())));
            }
        }
        let s = self.scale_factor;
        let r = info.rect;
        let size = (((r.w as f64 * s).round() as i32).max(1), ((r.h as f64 * s).round() as i32).max(1));
        self.casts.next_id += 1;
        let id = self.casts.next_id;
        let in_flight = Arc::new(AtomicBool::new(false));
        self.casts.send(ToPw::Start {
            id,
            width: size.0 as u32,
            height: size.1 as u32,
            in_flight: in_flight.clone(),
        });
        tracing::info!("Демонстрация экрана {id}: {} {}×{}", info.name, size.0, size.1);
        self.casts.casts.push(ActiveCast {
            id,
            monitor: info.name,
            cursor,
            size,
            rect: (r.x, r.y, r.w, r.h),
            reply: Some(reply),
            streaming: false,
            dirty: true,
            in_flight,
            offscreen: None,
            last: None,
        });
    }

    pub fn cast_stop(&mut self, id: u64) {
        let Some(i) = self.casts.casts.iter().position(|c| c.id == id) else { return };
        self.casts.casts.remove(i);
        for p in &mut self.casts.pending {
            p.cast_ids.retain(|&cid| cid != id);
        }
        self.casts.pending.retain(|p| !p.cast_ids.is_empty());
        self.casts.send(ToPw::Stop(id));
        tracing::info!("Демонстрация экрана {id} остановлена");
        self.shell.topbar_dirty = true;
        self.needs_redraw = true;
    }

    fn cast_event(&mut self, msg: FromPw) {
        match msg {
            FromPw::Node { id, node } => {
                let Some(cast) = self.casts.casts.iter_mut().find(|c| c.id == id) else { return };
                if let Some(reply) = cast.reply.take() {
                    reply.send(Ok(Reply::Cast { stream: id, node, rect: cast.rect }));
                }
            }
            FromPw::Streaming { id, on } => {
                let Some(cast) = self.casts.casts.iter_mut().find(|c| c.id == id) else { return };
                cast.streaming = on;
                // A new consumer needs a frame now, even if nothing on screen changes
                cast.dirty |= on;
                self.shell.topbar_dirty = true;
                self.needs_redraw = true;
                self.cast_capture_due();
            }
            FromPw::FrameDone { id } => {
                if self.casts.casts.iter().any(|c| c.id == id && c.streaming && c.dirty) {
                    self.cast_capture_due();
                }
            }
            FromPw::Failed { id, reason } => {
                tracing::warn!("Демонстрация экрана {id}: {reason}");
                if let Some(cast) = self.casts.casts.iter_mut().find(|c| c.id == id) {
                    if let Some(reply) = cast.reply.take() {
                        reply.send(Err(feather_api::Error::Failed(format!("PipeWire: {reason}"))));
                    }
                }
                self.cast_stop(id);
            }
            FromPw::BusDown(reason) => {
                self.casts.tx = None;
                self.casts.pending.clear();
                let mut casts = std::mem::take(&mut self.casts.casts);
                for cast in &mut casts {
                    if let Some(reply) = cast.reply.take() {
                        reply.send(Err(feather_api::Error::Unavailable(format!("PipeWire недоступен: {reason}"))));
                    }
                }
                if !casts.is_empty() {
                    self.shell.topbar_dirty = true;
                    self.needs_redraw = true;
                }
            }
        }
    }

    /// После кадра: мониторы `changed` (`None` — все) перерисовались
    pub fn cast_after_render(&mut self, changed: Option<&[String]>) {
        if self.casts.casts.is_empty() {
            return;
        }
        // The monitor was resized or removed: the negotiated size no longer fits
        let gone: Vec<u64> = self
            .casts
            .casts
            .iter()
            .filter(|c| {
                self.monitors.iter().find(|m| m.info.name == c.monitor).is_none_or(|m| {
                    let r = m.info.rect;
                    let s = self.scale_factor;
                    (r.x, r.y, r.w, r.h) != c.rect
                        || ((r.w as f64 * s).round() as i32, (r.h as f64 * s).round() as i32) != c.size
                })
            })
            .map(|c| c.id)
            .collect();
        for id in gone {
            self.cast_stop(id);
        }
        for cast in &mut self.casts.casts {
            if changed.is_none_or(|names| names.iter().any(|n| *n == cast.monitor)) {
                cast.dirty = true;
            }
        }
        self.cast_capture_due();
    }

    /// Снимает кадры тех потоков, чей монитор изменился, с дедупликацией одинаковых
    /// `(monitor, cursor)` и backpressure `in_flight` (не более 1 кадра в полёте на поток).
    fn cast_capture_due(&mut self) {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let mut ready_groups: Vec<(String, bool, (i32, i32), usize, Vec<u64>)> = Vec::new();
        for i in 0..self.casts.casts.len() {
            let cast = &self.casts.casts[i];
            if !cast.streaming || !cast.dirty || cast.in_flight.load(Ordering::Acquire) {
                continue;
            }
            if self.casts.pending.iter().any(|p| p.cast_ids.contains(&cast.id)) {
                continue;
            }
            let fps = cast_fps(cast.size.0, cast.size.1);
            let interval = Duration::from_nanos(1_000_000_000 / fps as u64);
            if let Some(at) = cast.last.map(|l| l + interval).filter(|at| *at > now) {
                next = Some(next.map_or(at, |n| n.min(at)));
                continue;
            }
            if let Some(group) = ready_groups
                .iter_mut()
                .find(|(m, cur, sz, _, _)| *m == cast.monitor && *cur == cast.cursor && *sz == cast.size)
            {
                group.4.push(cast.id);
            } else {
                ready_groups.push((cast.monitor.clone(), cast.cursor, cast.size, i, vec![cast.id]));
            }
        }
        for (monitor, cursor, size, leader_idx, cast_ids) in ready_groups {
            let mut target = self.casts.casts[leader_idx].offscreen.take();
            let mapping = self.render_cast_frame(&monitor, cursor, &mut target);
            self.casts.casts[leader_idx].offscreen = target;
            for cid in &cast_ids {
                if let Some(c) = self.casts.casts.iter_mut().find(|c| c.id == *cid) {
                    c.dirty = false;
                    c.last = Some(now);
                }
            }
            if let Some(mapping) = mapping {
                self.casts.pending.push(PendingCapture {
                    size,
                    cast_ids,
                    mapping,
                });
            }
        }
        if !self.casts.pending.is_empty() && !self.casts.flush_armed {
            self.casts.flush_armed = true;
            let _ = self.loop_handle.insert_source(Timer::from_duration(READBACK_DELAY), |_, _, state| {
                state.casts.flush_armed = false;
                state.cast_flush();
                TimeoutAction::Drop
            });
        }
        if let Some(at) = next {
            if self.casts.due_armed.is_none_or(|armed| armed > at) {
                self.casts.due_armed = Some(at);
                let _ = self.loop_handle.insert_source(Timer::from_deadline(at), move |_, _, state| {
                    if state.casts.due_armed == Some(at) {
                        state.casts.due_armed = None;
                    }
                    state.cast_capture_due();
                    TimeoutAction::Drop
                });
            }
        }
    }

    /// Готовые кадры из PBO → поток PipeWire напрямую через zero-copy `Arc<CastFrame>`
    fn cast_flush(&mut self) {
        let pending = std::mem::take(&mut self.casts.pending);
        for cap in pending {
            let need = (cap.size.0 as usize) * (cap.size.1 as usize) * 4;
            let Ok((ptr, len)) = self
                .backend
                .renderer()
                .map_texture(&cap.mapping)
                .map(|b| (b.as_ptr(), b.len()))
            else {
                continue;
            };
            if len < need {
                continue;
            }
            let frame = Arc::new(CastFrame {
                ptr,
                len: need,
                _mapping: cap.mapping,
            });
            for cid in cap.cast_ids {
                if let Some(cast) = self.casts.casts.iter().find(|c| c.id == cid && c.streaming) {
                    cast.in_flight.store(true, Ordering::Release);
                    self.casts.send(ToPw::Frame { id: cid, frame: frame.clone() });
                }
            }
        }
        // Changes that came while the readback was in flight
        self.cast_capture_due();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_serialize() {
        for obj in [format_param(1920, 1080), buffers_param(1920, 1080)] {
            let bytes = serialize(obj).expect("pod");
            assert!(spa::pod::Pod::from_bytes(&bytes).is_some());
        }
        // Regression test for OBS: SPA_PARAM_BUFFERS_dataType must serialize as SPA_CHOICE_Flags
        // so PipeWire intersects our MemFd|MemPtr (6) with OBS's MemFd|MemPtr|DmaBuf (14).
        let buf_bytes = serialize(buffers_param(3840, 2160)).expect("buffers pod");
        let (_, val) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(&buf_bytes)
            .expect("deserialize buffers pod");
        let spa::pod::Value::Object(obj) = val else {
            panic!("expected pod object");
        };
        let dt = obj
            .properties
            .iter()
            .find(|p| p.key == spa::sys::SPA_PARAM_BUFFERS_dataType)
            .expect("missing dataType prop");
        match &dt.value {
            spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                _,
                spa::utils::ChoiceEnum::Flags { default, .. },
            ))) => {
                let obs_mask = ((1 << spa::sys::SPA_DATA_MemFd)
                    | (1 << spa::sys::SPA_DATA_MemPtr)
                    | (1 << spa::sys::SPA_DATA_DmaBuf)) as i32;
                assert_ne!(*default & obs_mask, 0);
            }
            other => panic!("dataType must be ChoiceEnum::Flags, got {other:?}"),
        }
    }
}
