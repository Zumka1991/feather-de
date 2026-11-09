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
    time::{Duration, Instant},
};

use crate::{api::{Reply, Responder}, FeatherCompositor};

/// Наибольшая частота кадров, которую предлагаем потребителю
pub const MAX_FPS: u32 = 60;

pub enum ToPw {
    Start { id: u64, width: u32, height: u32 },
    /// Плотно упакованные строки RGBA сверху вниз, `width * height * 4` байт
    Frame { id: u64, rgba: Vec<u8> },
    Stop(u64),
}

#[derive(Debug)]
pub enum FromPw {
    /// Узел создан: его номер получает портал
    Node { id: u64, node: u32 },
    /// Потребитель подключился (`true`) или приостановил поток
    Streaming { id: u64, on: bool },
    Failed { id: u64, reason: String },
}

struct Shared {
    id: u64,
    width: u32,
    height: u32,
    events: channel::Sender<FromPw>,
    node_sent: bool,
    /// Последний кадр, ещё не отданный в буфер
    frame: Option<Vec<u8>>,
}

struct Cast {
    stream: pw::stream::StreamRc,
    shared: Rc<RefCell<Shared>>,
    _listener: pw::stream::StreamListener<Rc<RefCell<Shared>>>,
}

/// Запускает поток PipeWire; `None`, если PipeWire недоступен
pub fn spawn(events: channel::Sender<FromPw>) -> Option<pw::channel::Sender<ToPw>> {
    let (tx, rx) = pw::channel::channel::<ToPw>();
    let (ok_tx, ok_rx) = std::sync::mpsc::sync_channel::<bool>(1);
    let spawned = std::thread::Builder::new()
        .name("feather-pipewire".into())
        .stack_size(1024 * 1024)
        .spawn(move || {
            if let Err(e) = run(rx, events, &ok_tx) {
                tracing::warn!("PipeWire: {e}");
                let _ = ok_tx.try_send(false);
            }
        });
    if spawned.is_err() {
        return None;
    }
    // Only the connection is awaited (local socket), never a frame
    match ok_rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(true) => Some(tx),
        _ => None,
    }
}

fn run(
    rx: pw::channel::Receiver<ToPw>,
    events: channel::Sender<FromPw>,
    ok: &std::sync::mpsc::SyncSender<bool>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let _ = ok.try_send(true);

    let casts: Rc<RefCell<HashMap<u64, Cast>>> = Rc::new(RefCell::new(HashMap::new()));
    let casts_in = casts.clone();
    let _receiver = rx.attach(mainloop.loop_(), move |msg| match msg {
        ToPw::Start { id, width, height } => {
            match start(&core, id, width, height, events.clone()) {
                Ok(cast) => {
                    casts_in.borrow_mut().insert(id, cast);
                }
                Err(e) => {
                    let _ = events.send(FromPw::Failed { id, reason: e.to_string() });
                }
            }
        }
        ToPw::Frame { id, rgba } => {
            let casts = casts_in.borrow();
            let Some(cast) = casts.get(&id) else { return };
            cast.shared.borrow_mut().frame = Some(rgba);
            if cast.stream.state() == pw::stream::StreamState::Streaming {
                // We drive the graph: one cycle per new frame. Under another driver the
                // frame waits for its next process call.
                if cast.stream.is_driving() {
                    let _ = cast.stream.trigger_process();
                }
            }
        }
        ToPw::Stop(id) => {
            if let Some(cast) = casts_in.borrow_mut().remove(&id) {
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
        node_sent: false,
        frame: None,
    }));
    let listener = stream
        .add_local_listener_with_user_data(shared.clone())
        .state_changed(|stream, shared, _old, new| {
            let mut s = shared.borrow_mut();
            match new {
                pw::stream::StreamState::Paused => {
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
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else { return };
            let written = match data.data() {
                Some(dst) => {
                    let n = dst.len().min(frame.len()).min(size as usize);
                    dst[..n].copy_from_slice(&frame[..n]);
                    n as u32
                }
                None => 0,
            };
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = written;
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

/// Буферы в памяти (memfd): одна плоскость, строки без выравнивания
fn buffers_param(width: u32, height: u32) -> spa::pod::Object {
    use spa::pod::{Property, Value};
    use spa::sys;
    let stride = (width * 4) as i32;
    spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
        id: spa::param::ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(sys::SPA_PARAM_BUFFERS_buffers, choice_int(4, 2, 8)),
            Property::new(sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            Property::new(sys::SPA_PARAM_BUFFERS_size, Value::Int(stride * height as i32)),
            Property::new(sys::SPA_PARAM_BUFFERS_stride, Value::Int(stride)),
            Property::new(
                sys::SPA_PARAM_BUFFERS_dataType,
                Value::Int((1 << sys::SPA_DATA_MemFd) | (1 << sys::SPA_DATA_MemPtr)),
            ),
        ],
    }
}

/// Сколько ждём, пока видеокарта допишет кадр в PBO, прежде чем его прочитать
const READBACK_DELAY: Duration = Duration::from_millis(3);
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
    offscreen: Option<(GlesTexture, (i32, i32))>,
    pending: Option<GlesMapping>,
    last: Option<Instant>,
}

#[derive(Default)]
pub struct ScreenCasts {
    tx: Option<pw::channel::Sender<ToPw>>,
    casts: Vec<ActiveCast>,
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
        self.casts.send(ToPw::Start { id, width: size.0 as u32, height: size.1 as u32 });
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
            offscreen: None,
            pending: None,
            last: None,
        });
    }

    pub fn cast_stop(&mut self, id: u64) {
        let Some(i) = self.casts.casts.iter().position(|c| c.id == id) else { return };
        self.casts.casts.remove(i);
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
            FromPw::Failed { id, reason } => {
                tracing::warn!("Демонстрация экрана {id}: {reason}");
                if let Some(cast) = self.casts.casts.iter_mut().find(|c| c.id == id) {
                    if let Some(reply) = cast.reply.take() {
                        reply.send(Err(feather_api::Error::Failed(format!("PipeWire: {reason}"))));
                    }
                }
                self.cast_stop(id);
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

    /// Снимает кадры тех потоков, чей монитор изменился, не чаще `MAX_FPS`
    fn cast_capture_due(&mut self) {
        let interval = Duration::from_nanos(1_000_000_000 / MAX_FPS as u64);
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        for i in 0..self.casts.casts.len() {
            let cast = &self.casts.casts[i];
            if !cast.streaming || !cast.dirty || cast.pending.is_some() {
                continue;
            }
            if let Some(at) = cast.last.map(|l| l + interval).filter(|at| *at > now) {
                next = Some(next.map_or(at, |n| n.min(at)));
                continue;
            }
            let (monitor, cursor) = (cast.monitor.clone(), cast.cursor);
            let mut target = self.casts.casts[i].offscreen.take();
            let mapping = self.render_cast_frame(&monitor, cursor, &mut target);
            let cast = &mut self.casts.casts[i];
            cast.offscreen = target;
            cast.dirty = false;
            cast.last = Some(now);
            cast.pending = mapping;
        }
        if self.casts.casts.iter().any(|c| c.pending.is_some()) && !self.casts.flush_armed {
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

    /// Готовые кадры из PBO → поток PipeWire
    fn cast_flush(&mut self) {
        for i in 0..self.casts.casts.len() {
            let Some(mapping) = self.casts.casts[i].pending.take() else { continue };
            let (w, h) = self.casts.casts[i].size;
            let id = self.casts.casts[i].id;
            let Ok(bytes) = self.backend.renderer().map_texture(&mapping) else { continue };
            if bytes.len() < (w * h * 4) as usize {
                continue;
            }
            self.casts.send(ToPw::Frame { id, rgba: bytes[..(w * h * 4) as usize].to_vec() });
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
    }
}
