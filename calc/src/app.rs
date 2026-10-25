use std::borrow::Cow;
use feather_ui::i18n::tr;
use feather_ui::text::FontSystem;
use tiny_skia::{Color, PixmapMut};

use crate::theme::{
    colors, fill_circle, fill_rect, fill_rrect, line, rgba, stroke_rrect,
};

pub const TOOLBAR_H: f32 = 42.0;
pub const DISPLAY_H: f32 = 104.0;
pub const PROG_STRIP_H: f32 = 88.0;
pub const HISTORY_W: f32 = 196.0;
pub const RADIUS: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalcMode {
    Basic,
    Scientific,
    Programmer,
}

impl CalcMode {
    pub fn label(self) -> Cow<'static, str> {
        match self {
            Self::Basic => tr("Обычный"),
            Self::Scientific => tr("Инженерный"),
            Self::Programmer => tr("Программист"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgBase {
    Hex,
    Dec,
    Oct,
    Bin,
}

impl ProgBase {
    pub fn radix(self) -> u32 {
        match self {
            Self::Hex => 16,
            Self::Dec => 10,
            Self::Oct => 8,
            Self::Bin => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hex => "HEX",
            Self::Dec => "DEC",
            Self::Oct => "OCT",
            Self::Bin => "BIN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordSize {
    Bits64,
    Bits32,
    Bits16,
    Bits8,
}

impl WordSize {
    pub fn next(self) -> Self {
        match self {
            Self::Bits64 => Self::Bits32,
            Self::Bits32 => Self::Bits16,
            Self::Bits16 => Self::Bits8,
            Self::Bits8 => Self::Bits64,
        }
    }

    pub fn label(self) -> Cow<'static, str> {
        match self {
            Self::Bits64 => tr("64 бит"),
            Self::Bits32 => tr("32 бит"),
            Self::Bits16 => tr("16 бит"),
            Self::Bits8 => tr("8 бит"),
        }
    }

    pub fn mask(self, v: i64) -> u64 {
        let u = v as u64;
        match self {
            Self::Bits64 => u,
            Self::Bits32 => u & 0xFFFF_FFFF,
            Self::Bits16 => u & 0xFFFF,
            Self::Bits8 => u & 0xFF,
        }
    }

    pub fn sign_extend(self, u: u64) -> i64 {
        match self {
            Self::Bits64 => u as i64,
            Self::Bits32 => (u as u32 as i32) as i64,
            Self::Bits16 => (u as u16 as i16) as i64,
            Self::Bits8 => (u as u8 as i8) as i64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Root,
    EE,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "−",
            Self::Mul => "×",
            Self::Div => "÷",
            Self::Pow => "^",
            Self::Root => "√",
            Self::EE => "E",
            Self::BitAnd => "AND",
            Self::BitOr => "OR",
            Self::BitXor => "XOR",
            Self::Shl => "≪",
            Self::Shr => "≫",
        }
    }

    pub fn precedence(self) -> u8 {
        match self {
            Self::BitOr | Self::BitXor => 1,
            Self::BitAnd => 2,
            Self::Add | Self::Sub => 3,
            Self::Shl | Self::Shr => 4,
            Self::Mul | Self::Div => 5,
            Self::Pow | Self::Root | Self::EE => 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtnId {
    Digit(u8),
    Decimal,
    Clear,
    ToggleSign,
    Percent,
    Op(BinOp),
    Equals,
    ParenOpen,
    ParenClose,
    MemClear,
    MemAdd,
    MemSub,
    MemRecall,
    Second,
    Square,
    Cube,
    ExpE,
    Exp10,
    Recip,
    Sqrt,
    Cbrt,
    Ln,
    Log10,
    Fact,
    Sin,
    Cos,
    Tan,
    Sinh,
    Cosh,
    Tanh,
    ConstE,
    ConstPi,
    Rand,
    ToggleRad,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BtnRole {
    Digit,
    Utility,
    Operator,
    Scientific,
}

#[derive(Debug, Clone)]
struct BtnSpec {
    id: BtnId,
    label: String,
    role: BtnRole,
    col: usize,
    row: usize,
    col_span: usize,
    enabled: bool,
    active: bool,
}

#[derive(Debug, Clone)]
pub struct HistoryItem {
    pub expr: String,
    pub result: String,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Redraw,
    CloseWindow,
    MinimizeWindow,
    ToggleMaximize,
    DragWindow,
    Copy(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrafficHit {
    None,
    Close,
    Minimize,
    Maximize,
}

fn hit_traffic_lights(x: f32, y: f32) -> TrafficHit {
    let cy = TOOLBAR_H * 0.5;
    if (y - cy).abs() > 9.0 {
        return TrafficHit::None;
    }
    for (hit, cx) in [
        (TrafficHit::Close, 20.0),
        (TrafficHit::Minimize, 40.0),
        (TrafficHit::Maximize, 60.0),
    ] {
        if (x - cx).hypot(y - cy) <= 7.5 {
            return hit;
        }
    }
    TrafficHit::None
}

pub struct App {
    pub fonts: FontSystem,
    pub width: f32,
    pub height: f32,
    pub focused: bool,
    pub maximized: bool,
    pub dirty: bool,
    pub mode: CalcMode,
    pub show_history: bool,
    pub expr_line: String,
    pub input_buf: String,
    pub current_val: f64,
    pub entering: bool,
    pub error: Option<String>,
    pub val_stack: Vec<f64>,
    pub op_stack: Vec<Option<BinOp>>,
    pub last_op: Option<(BinOp, f64)>,
    pub memory: f64,
    pub radians: bool,
    pub second_fn: bool,
    pub prog_base: ProgBase,
    pub word_size: WordSize,
    pub history: Vec<HistoryItem>,
    pub hover_x: f32,
    pub hover_y: f32,
    pub active_btn: Option<BtnId>,
    rng_state: u64,
}

impl App {
    pub fn new(demo: bool) -> Self {
        let mut app = Self {
            fonts: FontSystem::new(),
            width: if demo { 720.0 } else { 360.0 },
            height: 500.0,
            focused: true,
            maximized: false,
            dirty: true,
            mode: CalcMode::Basic,
            show_history: false,
            expr_line: String::new(),
            input_buf: "0".to_string(),
            current_val: 0.0,
            entering: false,
            error: None,
            val_stack: Vec::new(),
            op_stack: Vec::new(),
            last_op: None,
            memory: 0.0,
            radians: false,
            second_fn: false,
            prog_base: ProgBase::Dec,
            word_size: WordSize::Bits64,
            history: Vec::new(),
            hover_x: -1.0,
            hover_y: -1.0,
            active_btn: None,
            rng_state: 0x9E37_79B9_7F4A_7C15,
        };
        if demo {
            app.mode = CalcMode::Scientific;
            app.show_history = true;
            app.history.push(HistoryItem {
                expr: "1 920 × 1 080".to_string(),
                result: "2 073 600".to_string(),
                value: 2_073_600.0,
            });
            app.history.push(HistoryItem {
                expr: "sin(30°) + 12²".to_string(),
                result: "144,5".to_string(),
                value: 144.5,
            });
            app.expr_line = "1 250 × 24 + 500 =".to_string();
            app.set_value(30_500.0);
        }
        app
    }

    pub fn set_size(&mut self, width: u32, height: u32) {
        let nw = (width as f32).max(280.0);
        let nh = (height as f32).max(400.0);
        if (nw - self.width).abs() > 0.5 || (nh - self.height).abs() > 0.5 {
            self.width = nw;
            self.height = nh;
            self.dirty = true;
        }
    }

    pub fn decimal_sep() -> char {
        match feather_ui::scale::lang() {
            feather_ui::i18n::Lang::En | feather_ui::i18n::Lang::Zh => '.',
            _ => ',',
        }
    }

    pub fn format_number(val: f64) -> String {
        if val.is_nan() || val.is_infinite() {
            return tr("Не число").into_owned();
        }
        let v = if val.abs() < 1e-12 { 0.0 } else { val };
        if v != 0.0 && (v.abs() >= 1e12 || v.abs() < 1e-7) {
            let s = format!("{v:.6e}");
            return s.replace('.', &Self::decimal_sep().to_string());
        }
        let rounded = (v * 1e10).round() / 1e10;
        let raw = format!("{rounded}");
        Self::format_raw_decimal(&raw)
    }

    fn format_raw_decimal(raw: &str) -> String {
        let (neg, rest) = match raw.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, raw),
        };
        let (int_part, frac_part) = match rest.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (rest, None),
        };
        let mut grouped = String::with_capacity(int_part.len() + int_part.len() / 3 + 4);
        if neg && rest != "0" {
            grouped.push('−');
        }
        let len = int_part.len();
        for (idx, ch) in int_part.chars().enumerate() {
            if idx > 0 && (len - idx) % 3 == 0 {
                grouped.push(' ');
            }
            grouped.push(ch);
        }
        if let Some(frac) = frac_part {
            grouped.push(Self::decimal_sep());
            grouped.push_str(frac);
        }
        grouped
    }

    pub fn format_prog_value(&self, base: ProgBase) -> String {
        let masked = self.word_size.mask(self.current_val as i64);
        match base {
            ProgBase::Hex => format!("{masked:X}"),
            ProgBase::Dec => {
                let signed = self.word_size.sign_extend(masked);
                Self::format_raw_decimal(&signed.to_string())
            }
            ProgBase::Oct => format!("{masked:o}"),
            ProgBase::Bin => {
                if masked == 0 {
                    return "0000".to_string();
                }
                let raw = format!("{masked:b}");
                let pad = (4 - (raw.len() % 4)) % 4;
                let mut full = String::with_capacity(raw.len() + pad);
                for _ in 0..pad {
                    full.push('0');
                }
                full.push_str(&raw);
                let mut out = String::new();
                for (i, ch) in full.chars().enumerate() {
                    if i > 0 && i % 4 == 0 {
                        out.push(' ');
                    }
                    out.push(ch);
                }
                out
            }
        }
    }

    pub fn display_text(&self) -> String {
        if let Some(err) = &self.error {
            return err.clone();
        }
        if self.mode == CalcMode::Programmer {
            if self.entering {
                return self.input_buf.clone();
            }
            return self.format_prog_value(self.prog_base);
        }
        if self.entering {
            Self::format_raw_decimal(&self.input_buf)
        } else {
            Self::format_number(self.current_val)
        }
    }

    fn set_value(&mut self, v: f64) {
        if v.is_nan() || v.is_infinite() {
            self.error = Some(tr("Ошибка").into_owned());
            self.current_val = 0.0;
            self.entering = false;
            return;
        }
        self.error = None;
        let normalized = if self.mode == CalcMode::Programmer {
            let masked = self.word_size.mask(v as i64);
            self.word_size.sign_extend(masked) as f64
        } else if v.abs() < 1e-14 {
            0.0
        } else {
            v
        };
        self.current_val = normalized;
        self.input_buf = format!("{normalized}");
        self.entering = false;
    }

    pub fn clear_all(&mut self) {
        self.expr_line.clear();
        self.input_buf = "0".to_string();
        self.current_val = 0.0;
        self.entering = false;
        self.error = None;
        self.val_stack.clear();
        self.op_stack.clear();
        self.last_op = None;
    }

    pub fn backspace(&mut self) {
        if self.error.is_some() {
            self.clear_all();
            return;
        }
        if !self.entering {
            return;
        }
        self.input_buf.pop();
        if self.input_buf.is_empty() || self.input_buf == "-" {
            self.input_buf = "0".to_string();
            self.current_val = 0.0;
            self.entering = false;
        } else {
            self.sync_value_from_input();
        }
    }

    fn sync_value_from_input(&mut self) {
        if self.mode == CalcMode::Programmer {
            let radix = self.prog_base.radix();
            let val = u64::from_str_radix(&self.input_buf, radix).unwrap_or(0);
            let masked = self.word_size.mask(val as i64);
            self.current_val = self.word_size.sign_extend(masked) as f64;
        } else {
            self.current_val = self.input_buf.parse::<f64>().unwrap_or(0.0);
        }
    }

    pub fn push_digit(&mut self, d: u8) {
        if self.error.is_some() {
            self.clear_all();
        }
        if self.mode == CalcMode::Programmer {
            if u32::from(d) >= self.prog_base.radix() {
                return;
            }
            let ch = char::from_digit(u32::from(d), 16)
                .unwrap_or('0')
                .to_ascii_uppercase();
            if !self.entering || self.input_buf == "0" {
                self.input_buf.clear();
                self.entering = true;
            }
            if self.input_buf.len() < 16 {
                self.input_buf.push(ch);
                self.sync_value_from_input();
            }
            return;
        }
        if d > 9 {
            return;
        }
        let ch = (b'0' + d) as char;
        if !self.entering || self.input_buf == "0" {
            self.input_buf = ch.to_string();
            self.entering = true;
        } else if self.input_buf == "-0" {
            self.input_buf = format!("-{ch}");
        } else if self.input_buf.len() < 16 {
            self.input_buf.push(ch);
        }
        self.sync_value_from_input();
    }

    pub fn push_decimal(&mut self) {
        if self.mode == CalcMode::Programmer {
            return;
        }
        if self.error.is_some() {
            self.clear_all();
        }
        if !self.entering {
            self.input_buf = "0.".to_string();
            self.current_val = 0.0;
            self.entering = true;
        } else if !self.input_buf.contains('.') {
            self.input_buf.push('.');
        }
    }

    fn apply_bin_op(&self, op: BinOp, a: f64, b: f64) -> Option<f64> {
        match op {
            BinOp::Add => Some(a + b),
            BinOp::Sub => Some(a - b),
            BinOp::Mul => Some(a * b),
            BinOp::Div => {
                if b == 0.0 {
                    None
                } else if self.mode == CalcMode::Programmer {
                    Some(((a as i64) / (b as i64)) as f64)
                } else {
                    Some(a / b)
                }
            }
            BinOp::Pow => Some(a.powf(b)),
            BinOp::Root => {
                if b == 0.0 {
                    None
                } else {
                    Some(a.powf(1.0 / b))
                }
            }
            BinOp::EE => Some(a * 10.0_f64.powf(b)),
            BinOp::BitAnd => Some(((a as i64) & (b as i64)) as f64),
            BinOp::BitOr => Some(((a as i64) | (b as i64)) as f64),
            BinOp::BitXor => Some(((a as i64) ^ (b as i64)) as f64),
            BinOp::Shl => Some((a as i64).wrapping_shl((b as u32) & 63) as f64),
            BinOp::Shr => Some((a as i64).wrapping_shr((b as u32) & 63) as f64),
        }
    }

    fn reduce_with_precedence(&mut self, min_prec: u8) -> bool {
        while let Some(Some(top_op)) = self.op_stack.last().copied() {
            if top_op.precedence() < min_prec {
                break;
            }
            self.op_stack.pop();
            let lhs = self.val_stack.pop().unwrap_or(0.0);
            let rhs = self.current_val;
            match self.apply_bin_op(top_op, lhs, rhs) {
                Some(res) => {
                    self.set_value(res);
                }
                None => {
                    self.error = Some(tr("Деление на 0").into_owned());
                    self.val_stack.clear();
                    self.op_stack.clear();
                    return false;
                }
            }
        }
        true
    }

    pub fn push_op(&mut self, op: BinOp) {
        if self.error.is_some() {
            return;
        }
        self.last_op = None;
        if !self.entering && matches!(self.op_stack.last(), Some(Some(_))) {
            *self.op_stack.last_mut().unwrap() = Some(op);
            self.rebuild_expr_preview();
            return;
        }
        if !self.reduce_with_precedence(op.precedence()) {
            return;
        }
        self.val_stack.push(self.current_val);
        self.op_stack.push(Some(op));
        self.entering = false;
        self.rebuild_expr_preview();
    }

    pub fn open_paren(&mut self) {
        if self.error.is_some() {
            self.clear_all();
        }
        self.op_stack.push(None);
        self.entering = false;
        self.rebuild_expr_preview();
    }

    pub fn close_paren(&mut self) {
        if self.error.is_some() {
            return;
        }
        if !self.op_stack.iter().any(Option::is_none) {
            return;
        }
        if !self.reduce_with_precedence(1) {
            return;
        }
        if matches!(self.op_stack.last(), Some(None)) {
            self.op_stack.pop();
        }
        self.rebuild_expr_preview();
    }

    pub fn evaluate(&mut self) {
        if self.error.is_some() {
            return;
        }
        if self.op_stack.is_empty() {
            if let Some((op, rhs)) = self.last_op {
                let lhs = self.current_val;
                let expr = format!(
                    "{} {} {}",
                    self.format_operand(lhs),
                    op.symbol(),
                    self.format_operand(rhs)
                );
                if let Some(res) = self.apply_bin_op(op, lhs, rhs) {
                    self.set_value(res);
                    let res_str = self.display_text();
                    self.expr_line = format!("{expr} =");
                    self.push_history(expr, res_str, self.current_val);
                } else {
                    self.error = Some(tr("Деление на 0").into_owned());
                }
            }
            return;
        }

        let rhs_snapshot = self.current_val;
        let last_bin = self.op_stack.iter().rev().flatten().copied().next();
        let full_expr = self.compose_full_expr();

        while !self.op_stack.is_empty() {
            if !self.reduce_with_precedence(1) {
                return;
            }
            if matches!(self.op_stack.last(), Some(None)) {
                self.op_stack.pop();
            }
        }
        if let Some(op) = last_bin {
            self.last_op = Some((op, rhs_snapshot));
        }
        let result_str = self.display_text();
        self.expr_line = format!("{full_expr} =");
        self.push_history(full_expr, result_str, self.current_val);
    }

    fn format_operand(&self, v: f64) -> String {
        if self.mode == CalcMode::Programmer {
            let masked = self.word_size.mask(v as i64);
            match self.prog_base {
                ProgBase::Hex => format!("0x{masked:X}"),
                ProgBase::Dec => Self::format_raw_decimal(&self.word_size.sign_extend(masked).to_string()),
                ProgBase::Oct => format!("0o{masked:o}"),
                ProgBase::Bin => format!("0b{masked:b}"),
            }
        } else {
            Self::format_number(v)
        }
    }

    fn rebuild_expr_preview(&mut self) {
        let mut s = String::new();
        let mut val_idx = 0;
        for item in &self.op_stack {
            match item {
                None => s.push('('),
                Some(op) => {
                    if let Some(&v) = self.val_stack.get(val_idx) {
                        val_idx += 1;
                        if !s.is_empty() && !s.ends_with('(') {
                            s.push(' ');
                        }
                        s.push_str(&self.format_operand(v));
                    }
                    s.push(' ');
                    s.push_str(op.symbol());
                }
            }
        }
        self.expr_line = s;
    }

    fn compose_full_expr(&self) -> String {
        let mut s = self.expr_line.clone();
        if !s.is_empty() && !s.ends_with('(') {
            s.push(' ');
        }
        s.push_str(&self.format_operand(self.current_val));
        let open_parens = self.op_stack.iter().filter(|x| x.is_none()).count();
        for _ in 0..open_parens {
            s.push(')');
        }
        s
    }

    fn push_history(&mut self, expr: String, result: String, value: f64) {
        if self.history.len() >= 40 {
            self.history.remove(0);
        }
        self.history.push(HistoryItem {
            expr,
            result,
            value,
        });
    }

    fn to_angle(&self, x: f64) -> f64 {
        if self.radians {
            x
        } else {
            x.to_radians()
        }
    }

    fn from_angle(&self, rad: f64) -> f64 {
        if self.radians {
            rad
        } else {
            rad.to_degrees()
        }
    }

    pub fn press_button(&mut self, btn: BtnId) {
        self.active_btn = Some(btn);
        self.dirty = true;
        match btn {
            BtnId::Digit(d) => self.push_digit(d),
            BtnId::Decimal => self.push_decimal(),
            BtnId::Clear => {
                if self.entering && self.input_buf != "0" {
                    self.backspace();
                } else {
                    self.clear_all();
                }
            }
            BtnId::ToggleSign => {
                if self.entering && self.mode != CalcMode::Programmer {
                    if self.input_buf.starts_with('-') {
                        self.input_buf.remove(0);
                    } else if self.input_buf != "0" {
                        self.input_buf.insert(0, '-');
                    }
                    self.sync_value_from_input();
                } else {
                    self.set_value(-self.current_val);
                }
            }
            BtnId::Percent => {
                let base = self.val_stack.last().copied().unwrap_or(1.0);
                let pct = if matches!(self.op_stack.last(), Some(Some(BinOp::Add | BinOp::Sub))) {
                    base * (self.current_val / 100.0)
                } else {
                    self.current_val / 100.0
                };
                self.set_value(pct);
            }
            BtnId::Op(op) => self.push_op(op),
            BtnId::Equals => self.evaluate(),
            BtnId::ParenOpen => self.open_paren(),
            BtnId::ParenClose => self.close_paren(),
            BtnId::MemClear => self.memory = 0.0,
            BtnId::MemAdd => self.memory += self.current_val,
            BtnId::MemSub => self.memory -= self.current_val,
            BtnId::MemRecall => self.set_value(self.memory),
            BtnId::Second => self.second_fn = !self.second_fn,
            BtnId::ToggleRad => self.radians = !self.radians,
            BtnId::Square => {
                let x = self.current_val;
                self.expr_line = format!("({})²", Self::format_number(x));
                self.set_value(x * x);
            }
            BtnId::Cube => {
                let x = self.current_val;
                self.expr_line = format!("({})³", Self::format_number(x));
                self.set_value(x * x * x);
            }
            BtnId::ExpE => {
                let x = self.current_val;
                if self.second_fn {
                    self.expr_line = format!("2^({})", Self::format_number(x));
                    self.set_value(2.0_f64.powf(x));
                } else {
                    self.expr_line = format!("e^({})", Self::format_number(x));
                    self.set_value(x.exp());
                }
            }
            BtnId::Exp10 => {
                let x = self.current_val;
                self.expr_line = format!("10^({})", Self::format_number(x));
                self.set_value(10.0_f64.powf(x));
            }
            BtnId::Recip => {
                let x = self.current_val;
                if x == 0.0 {
                    self.error = Some(tr("Деление на 0").into_owned());
                } else {
                    self.expr_line = format!("1/({})", Self::format_number(x));
                    self.set_value(1.0 / x);
                }
            }
            BtnId::Sqrt => {
                let x = self.current_val;
                if x < 0.0 {
                    self.error = Some(tr("Ошибка").into_owned());
                } else {
                    self.expr_line = format!("√({})", Self::format_number(x));
                    self.set_value(x.sqrt());
                }
            }
            BtnId::Cbrt => {
                let x = self.current_val;
                self.expr_line = format!("∛({})", Self::format_number(x));
                self.set_value(x.cbrt());
            }
            BtnId::Ln => {
                let x = self.current_val;
                if x <= 0.0 {
                    self.error = Some(tr("Ошибка").into_owned());
                } else if self.second_fn {
                    self.expr_line = format!("log₂({})", Self::format_number(x));
                    self.set_value(x.log2());
                } else {
                    self.expr_line = format!("ln({})", Self::format_number(x));
                    self.set_value(x.ln());
                }
            }
            BtnId::Log10 => {
                let x = self.current_val;
                if x <= 0.0 {
                    self.error = Some(tr("Ошибка").into_owned());
                } else {
                    self.expr_line = format!("log₁₀({})", Self::format_number(x));
                    self.set_value(x.log10());
                }
            }
            BtnId::Fact => {
                let x = self.current_val;
                if x < 0.0 || x.fract() != 0.0 || x > 170.0 {
                    self.error = Some(tr("Ошибка").into_owned());
                } else {
                    let n = x as u64;
                    let mut acc = 1.0_f64;
                    for i in 2..=n {
                        acc *= i as f64;
                    }
                    self.expr_line = format!("{n}!");
                    self.set_value(acc);
                }
            }
            BtnId::Sin => {
                let x = self.current_val;
                if self.second_fn {
                    self.expr_line = format!("arcsin({})", Self::format_number(x));
                    self.set_value(self.from_angle(x.asin()));
                } else {
                    let unit = if self.radians { " рад" } else { "°" };
                    self.expr_line = format!("sin({}{unit})", Self::format_number(x));
                    self.set_value(self.to_angle(x).sin());
                }
            }
            BtnId::Cos => {
                let x = self.current_val;
                if self.second_fn {
                    self.expr_line = format!("arccos({})", Self::format_number(x));
                    self.set_value(self.from_angle(x.acos()));
                } else {
                    let unit = if self.radians { " рад" } else { "°" };
                    self.expr_line = format!("cos({}{unit})", Self::format_number(x));
                    self.set_value(self.to_angle(x).cos());
                }
            }
            BtnId::Tan => {
                let x = self.current_val;
                if self.second_fn {
                    self.expr_line = format!("arctan({})", Self::format_number(x));
                    self.set_value(self.from_angle(x.atan()));
                } else {
                    let unit = if self.radians { " рад" } else { "°" };
                    self.expr_line = format!("tan({}{unit})", Self::format_number(x));
                    self.set_value(self.to_angle(x).tan());
                }
            }
            BtnId::Sinh => {
                let x = self.current_val;
                let v = if self.second_fn { x.asinh() } else { x.sinh() };
                self.set_value(v);
            }
            BtnId::Cosh => {
                let x = self.current_val;
                let v = if self.second_fn { x.acosh() } else { x.cosh() };
                self.set_value(v);
            }
            BtnId::Tanh => {
                let x = self.current_val;
                let v = if self.second_fn { x.atanh() } else { x.tanh() };
                self.set_value(v);
            }
            BtnId::ConstE => self.set_value(std::f64::consts::E),
            BtnId::ConstPi => self.set_value(std::f64::consts::PI),
            BtnId::Rand => {
                self.rng_state = self
                    .rng_state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let r = ((self.rng_state >> 11) as f64) * (1.0 / ((1u64 << 53) as f64));
                self.set_value((r * 1e6).round() / 1e6);
            }
            BtnId::BitNot => {
                let v = !(self.current_val as i64);
                self.set_value(v as f64);
            }
        }
    }

    pub fn paste_text(&mut self, text: &str) {
        let cleaned: String = text
            .trim()
            .replace(' ', "")
            .replace('\u{00A0}', "")
            .replace('−', "-")
            .replace(',', ".");
        if self.mode == CalcMode::Programmer {
            let stripped = cleaned
                .strip_prefix("0x")
                .or_else(|| cleaned.strip_prefix("0X"))
                .unwrap_or(&cleaned);
            if let Ok(v) = u64::from_str_radix(stripped, self.prog_base.radix()) {
                let masked = self.word_size.mask(v as i64);
                self.set_value(self.word_size.sign_extend(masked) as f64);
                self.dirty = true;
            }
        } else if let Ok(v) = cleaned.parse::<f64>() {
            self.set_value(v);
            self.dirty = true;
        }
    }

    pub fn handle_key(
        &mut self,
        keysym: u32,
        utf8: Option<&str>,
        ctrl: bool,
    ) -> Action {
        use smithay_client_toolkit::seat::keyboard::Keysym;
        if ctrl {
            match Keysym::new(keysym) {
                Keysym::c | Keysym::C => return Action::Copy(self.display_text()),
                Keysym::h | Keysym::H | Keysym::t | Keysym::T => {
                    self.show_history = !self.show_history;
                    self.dirty = true;
                    return Action::Redraw;
                }
                Keysym::_1 => {
                    self.set_mode(CalcMode::Basic);
                    return Action::Redraw;
                }
                Keysym::_2 => {
                    self.set_mode(CalcMode::Scientific);
                    return Action::Redraw;
                }
                Keysym::_3 => {
                    self.set_mode(CalcMode::Programmer);
                    return Action::Redraw;
                }
                _ => {}
            }
        }

        match Keysym::new(keysym) {
            Keysym::Return | Keysym::KP_Enter => {
                self.press_button(BtnId::Equals);
                return Action::Redraw;
            }
            Keysym::BackSpace => {
                self.backspace();
                self.dirty = true;
                return Action::Redraw;
            }
            Keysym::Escape | Keysym::Delete => {
                self.clear_all();
                self.dirty = true;
                return Action::Redraw;
            }
            _ => {}
        }

        if let Some(s) = utf8 {
            for ch in s.chars() {
                match ch {
                    '0'..='9' => self.push_digit((ch as u8) - b'0'),
                    'a'..='f' if self.mode == CalcMode::Programmer => {
                        self.push_digit(10 + (ch as u8) - b'a')
                    }
                    'A'..='F' if self.mode == CalcMode::Programmer => {
                        self.push_digit(10 + (ch as u8) - b'A')
                    }
                    '.' | ',' => self.push_decimal(),
                    '+' => self.push_op(BinOp::Add),
                    '-' => self.push_op(BinOp::Sub),
                    '*' => self.push_op(BinOp::Mul),
                    '/' => self.push_op(BinOp::Div),
                    '^' => self.push_op(BinOp::Pow),
                    '%' => self.press_button(BtnId::Percent),
                    '(' => self.open_paren(),
                    ')' => self.close_paren(),
                    '=' => self.evaluate(),
                    _ => {}
                }
            }
            self.dirty = true;
            return Action::Redraw;
        }
        Action::None
    }

    pub fn set_mode(&mut self, mode: CalcMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == CalcMode::Programmer {
            self.set_value(self.current_val.trunc());
        }
        self.dirty = true;
    }

    fn clear_label(&self) -> &'static str {
        if self.entering && self.input_buf != "0" {
            "⌫"
        } else {
            "AC"
        }
    }

    fn is_pending_op(&self, op: BinOp) -> bool {
        !self.entering && matches!(self.op_stack.last(), Some(Some(top)) if *top == op)
    }

    fn keypad_specs(&self) -> (usize, usize, Vec<BtnSpec>) {
        let dec_label = Self::decimal_sep().to_string();
        match self.mode {
            CalcMode::Basic => {
                let specs = vec![
                    BtnSpec { id: BtnId::Clear, label: self.clear_label().into(), role: BtnRole::Utility, col: 0, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::ToggleSign, label: "+/−".into(), role: BtnRole::Utility, col: 1, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Percent, label: "%".into(), role: BtnRole::Utility, col: 2, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Div), label: "÷".into(), role: BtnRole::Operator, col: 3, row: 0, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Div) },

                    BtnSpec { id: BtnId::Digit(7), label: "7".into(), role: BtnRole::Digit, col: 0, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(8), label: "8".into(), role: BtnRole::Digit, col: 1, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(9), label: "9".into(), role: BtnRole::Digit, col: 2, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Mul), label: "×".into(), role: BtnRole::Operator, col: 3, row: 1, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Mul) },

                    BtnSpec { id: BtnId::Digit(4), label: "4".into(), role: BtnRole::Digit, col: 0, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(5), label: "5".into(), role: BtnRole::Digit, col: 1, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(6), label: "6".into(), role: BtnRole::Digit, col: 2, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Sub), label: "−".into(), role: BtnRole::Operator, col: 3, row: 2, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Sub) },

                    BtnSpec { id: BtnId::Digit(1), label: "1".into(), role: BtnRole::Digit, col: 0, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(2), label: "2".into(), role: BtnRole::Digit, col: 1, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(3), label: "3".into(), role: BtnRole::Digit, col: 2, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Add), label: "+".into(), role: BtnRole::Operator, col: 3, row: 3, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Add) },

                    BtnSpec { id: BtnId::Digit(0), label: "0".into(), role: BtnRole::Digit, col: 0, row: 4, col_span: 2, enabled: true, active: false },
                    BtnSpec { id: BtnId::Decimal, label: dec_label, role: BtnRole::Digit, col: 2, row: 4, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Equals, label: "=".into(), role: BtnRole::Operator, col: 3, row: 4, col_span: 1, enabled: true, active: false },
                ];
                (4, 5, specs)
            }
            CalcMode::Scientific => {
                let sin_l = if self.second_fn { "sin⁻¹" } else { "sin" };
                let cos_l = if self.second_fn { "cos⁻¹" } else { "cos" };
                let tan_l = if self.second_fn { "tan⁻¹" } else { "tan" };
                let sinh_l = if self.second_fn { "sinh⁻¹" } else { "sinh" };
                let cosh_l = if self.second_fn { "cosh⁻¹" } else { "cosh" };
                let tanh_l = if self.second_fn { "tanh⁻¹" } else { "tanh" };
                let exp_l = if self.second_fn { "2ˣ" } else { "eˣ" };
                let ln_l = if self.second_fn { "log₂" } else { "ln" };
                let rad_l = if self.radians { "Deg" } else { "Rad" };
                let specs = vec![
                    BtnSpec { id: BtnId::ParenOpen, label: "(".into(), role: BtnRole::Scientific, col: 0, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::ParenClose, label: ")".into(), role: BtnRole::Scientific, col: 1, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::MemClear, label: "mc".into(), role: BtnRole::Scientific, col: 2, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::MemAdd, label: "m+".into(), role: BtnRole::Scientific, col: 3, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::MemSub, label: "m−".into(), role: BtnRole::Scientific, col: 4, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::MemRecall, label: "mr".into(), role: BtnRole::Scientific, col: 5, row: 0, col_span: 1, enabled: true, active: self.memory != 0.0 },
                    BtnSpec { id: BtnId::Clear, label: self.clear_label().into(), role: BtnRole::Utility, col: 6, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::ToggleSign, label: "+/−".into(), role: BtnRole::Utility, col: 7, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Percent, label: "%".into(), role: BtnRole::Utility, col: 8, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Div), label: "÷".into(), role: BtnRole::Operator, col: 9, row: 0, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Div) },

                    BtnSpec { id: BtnId::Second, label: "2nd".into(), role: BtnRole::Scientific, col: 0, row: 1, col_span: 1, enabled: true, active: self.second_fn },
                    BtnSpec { id: BtnId::Square, label: "x²".into(), role: BtnRole::Scientific, col: 1, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Cube, label: "x³".into(), role: BtnRole::Scientific, col: 2, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Pow), label: "xʸ".into(), role: BtnRole::Scientific, col: 3, row: 1, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Pow) },
                    BtnSpec { id: BtnId::ExpE, label: exp_l.into(), role: BtnRole::Scientific, col: 4, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Exp10, label: "10ˣ".into(), role: BtnRole::Scientific, col: 5, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(7), label: "7".into(), role: BtnRole::Digit, col: 6, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(8), label: "8".into(), role: BtnRole::Digit, col: 7, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(9), label: "9".into(), role: BtnRole::Digit, col: 8, row: 1, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Mul), label: "×".into(), role: BtnRole::Operator, col: 9, row: 1, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Mul) },

                    BtnSpec { id: BtnId::Recip, label: "1/x".into(), role: BtnRole::Scientific, col: 0, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Sqrt, label: "√x".into(), role: BtnRole::Scientific, col: 1, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Cbrt, label: "∛x".into(), role: BtnRole::Scientific, col: 2, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Root), label: "ʸ√x".into(), role: BtnRole::Scientific, col: 3, row: 2, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Root) },
                    BtnSpec { id: BtnId::Ln, label: ln_l.into(), role: BtnRole::Scientific, col: 4, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Log10, label: "log₁₀".into(), role: BtnRole::Scientific, col: 5, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(4), label: "4".into(), role: BtnRole::Digit, col: 6, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(5), label: "5".into(), role: BtnRole::Digit, col: 7, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(6), label: "6".into(), role: BtnRole::Digit, col: 8, row: 2, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Sub), label: "−".into(), role: BtnRole::Operator, col: 9, row: 2, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Sub) },

                    BtnSpec { id: BtnId::Fact, label: "x!".into(), role: BtnRole::Scientific, col: 0, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Sin, label: sin_l.into(), role: BtnRole::Scientific, col: 1, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Cos, label: cos_l.into(), role: BtnRole::Scientific, col: 2, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Tan, label: tan_l.into(), role: BtnRole::Scientific, col: 3, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::ConstE, label: "e".into(), role: BtnRole::Scientific, col: 4, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::EE), label: "EE".into(), role: BtnRole::Scientific, col: 5, row: 3, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::EE) },
                    BtnSpec { id: BtnId::Digit(1), label: "1".into(), role: BtnRole::Digit, col: 6, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(2), label: "2".into(), role: BtnRole::Digit, col: 7, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(3), label: "3".into(), role: BtnRole::Digit, col: 8, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Add), label: "+".into(), role: BtnRole::Operator, col: 9, row: 3, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Add) },

                    BtnSpec { id: BtnId::ToggleRad, label: rad_l.into(), role: BtnRole::Scientific, col: 0, row: 4, col_span: 1, enabled: true, active: self.radians },
                    BtnSpec { id: BtnId::Sinh, label: sinh_l.into(), role: BtnRole::Scientific, col: 1, row: 4, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Cosh, label: cosh_l.into(), role: BtnRole::Scientific, col: 2, row: 4, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Tanh, label: tanh_l.into(), role: BtnRole::Scientific, col: 3, row: 4, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::ConstPi, label: "π".into(), role: BtnRole::Scientific, col: 4, row: 4, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Rand, label: "Rand".into(), role: BtnRole::Scientific, col: 5, row: 4, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(0), label: "0".into(), role: BtnRole::Digit, col: 6, row: 4, col_span: 2, enabled: true, active: false },
                    BtnSpec { id: BtnId::Decimal, label: dec_label, role: BtnRole::Digit, col: 8, row: 4, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Equals, label: "=".into(), role: BtnRole::Operator, col: 9, row: 4, col_span: 1, enabled: true, active: false },
                ];
                (10, 5, specs)
            }
            CalcMode::Programmer => {
                let r = self.prog_base.radix();
                let specs = vec![
                    BtnSpec { id: BtnId::Digit(10), label: "A".into(), role: BtnRole::Scientific, col: 0, row: 0, col_span: 1, enabled: r > 10, active: false },
                    BtnSpec { id: BtnId::Digit(11), label: "B".into(), role: BtnRole::Scientific, col: 1, row: 0, col_span: 1, enabled: r > 11, active: false },
                    BtnSpec { id: BtnId::Clear, label: self.clear_label().into(), role: BtnRole::Utility, col: 2, row: 0, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Shl), label: "≪".into(), role: BtnRole::Utility, col: 3, row: 0, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Shl) },
                    BtnSpec { id: BtnId::Op(BinOp::Shr), label: "≫".into(), role: BtnRole::Utility, col: 4, row: 0, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Shr) },
                    BtnSpec { id: BtnId::Op(BinOp::Div), label: "÷".into(), role: BtnRole::Operator, col: 5, row: 0, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Div) },

                    BtnSpec { id: BtnId::Digit(12), label: "C".into(), role: BtnRole::Scientific, col: 0, row: 1, col_span: 1, enabled: r > 12, active: false },
                    BtnSpec { id: BtnId::Digit(13), label: "D".into(), role: BtnRole::Scientific, col: 1, row: 1, col_span: 1, enabled: r > 13, active: false },
                    BtnSpec { id: BtnId::Digit(7), label: "7".into(), role: BtnRole::Digit, col: 2, row: 1, col_span: 1, enabled: r > 7, active: false },
                    BtnSpec { id: BtnId::Digit(8), label: "8".into(), role: BtnRole::Digit, col: 3, row: 1, col_span: 1, enabled: r > 8, active: false },
                    BtnSpec { id: BtnId::Digit(9), label: "9".into(), role: BtnRole::Digit, col: 4, row: 1, col_span: 1, enabled: r > 9, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Mul), label: "×".into(), role: BtnRole::Operator, col: 5, row: 1, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Mul) },

                    BtnSpec { id: BtnId::Digit(14), label: "E".into(), role: BtnRole::Scientific, col: 0, row: 2, col_span: 1, enabled: r > 14, active: false },
                    BtnSpec { id: BtnId::Digit(15), label: "F".into(), role: BtnRole::Scientific, col: 1, row: 2, col_span: 1, enabled: r > 15, active: false },
                    BtnSpec { id: BtnId::Digit(4), label: "4".into(), role: BtnRole::Digit, col: 2, row: 2, col_span: 1, enabled: r > 4, active: false },
                    BtnSpec { id: BtnId::Digit(5), label: "5".into(), role: BtnRole::Digit, col: 3, row: 2, col_span: 1, enabled: r > 5, active: false },
                    BtnSpec { id: BtnId::Digit(6), label: "6".into(), role: BtnRole::Digit, col: 4, row: 2, col_span: 1, enabled: r > 6, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Sub), label: "−".into(), role: BtnRole::Operator, col: 5, row: 2, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Sub) },

                    BtnSpec { id: BtnId::Op(BinOp::BitAnd), label: "AND".into(), role: BtnRole::Scientific, col: 0, row: 3, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::BitAnd) },
                    BtnSpec { id: BtnId::Op(BinOp::BitOr), label: "OR".into(), role: BtnRole::Scientific, col: 1, row: 3, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::BitOr) },
                    BtnSpec { id: BtnId::Digit(1), label: "1".into(), role: BtnRole::Digit, col: 2, row: 3, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(2), label: "2".into(), role: BtnRole::Digit, col: 3, row: 3, col_span: 1, enabled: r > 2, active: false },
                    BtnSpec { id: BtnId::Digit(3), label: "3".into(), role: BtnRole::Digit, col: 4, row: 3, col_span: 1, enabled: r > 3, active: false },
                    BtnSpec { id: BtnId::Op(BinOp::Add), label: "+".into(), role: BtnRole::Operator, col: 5, row: 3, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::Add) },

                    BtnSpec { id: BtnId::Op(BinOp::BitXor), label: "XOR".into(), role: BtnRole::Scientific, col: 0, row: 4, col_span: 1, enabled: true, active: self.is_pending_op(BinOp::BitXor) },
                    BtnSpec { id: BtnId::BitNot, label: "NOT".into(), role: BtnRole::Scientific, col: 1, row: 4, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Digit(0), label: "0".into(), role: BtnRole::Digit, col: 2, row: 4, col_span: 2, enabled: true, active: false },
                    BtnSpec { id: BtnId::ToggleSign, label: "+/−".into(), role: BtnRole::Digit, col: 4, row: 4, col_span: 1, enabled: true, active: false },
                    BtnSpec { id: BtnId::Equals, label: "=".into(), role: BtnRole::Operator, col: 5, row: 4, col_span: 1, enabled: true, active: false },
                ];
                (6, 5, specs)
            }
        }
    }

    fn main_x(&self) -> f32 {
        if self.show_history {
            HISTORY_W
        } else {
            0.0
        }
    }

    fn main_w(&self) -> f32 {
        (self.width - self.main_x()).max(220.0)
    }

    fn keypad_rect(&self) -> (f32, f32, f32, f32) {
        let x0 = self.main_x() + 12.0;
        let w = (self.main_w() - 24.0).max(160.0);
        let y0 = TOOLBAR_H
            + DISPLAY_H
            + if self.mode == CalcMode::Programmer {
                PROG_STRIP_H
            } else {
                0.0
            };
        let h = (self.height - y0 - 12.0).max(160.0);
        (x0, y0, w, h)
    }

    pub fn handle_pointer_motion(&mut self, x: f32, y: f32) -> Action {
        if (self.hover_x - x).abs() > 0.5 || (self.hover_y - y).abs() > 0.5 {
            self.hover_x = x;
            self.hover_y = y;
            self.dirty = true;
            return Action::Redraw;
        }
        Action::None
    }

    pub fn handle_click(&mut self, x: f32, y: f32) -> Action {
        match hit_traffic_lights(x, y) {
            TrafficHit::Close => return Action::CloseWindow,
            TrafficHit::Minimize => return Action::MinimizeWindow,
            TrafficHit::Maximize => return Action::ToggleMaximize,
            TrafficHit::None => {}
        }

        if y < TOOLBAR_H {
            let hist_x = self.width - 40.0;
            if (hist_x..=hist_x + 28.0).contains(&x) && (8.0..=34.0).contains(&y) {
                self.show_history = !self.show_history;
                self.dirty = true;
                return Action::Redraw;
            }

            let seg_w = 246.0_f32.min(self.width - 140.0).max(168.0);
            let seg_x = ((self.width - seg_w) * 0.5).max(74.0);
            let item_w = seg_w / 3.0;
            if (seg_x..=seg_x + seg_w).contains(&x) && (8.0..=34.0).contains(&y) {
                let idx = ((x - seg_x) / item_w).floor() as usize;
                let mode = match idx {
                    0 => CalcMode::Basic,
                    1 => CalcMode::Scientific,
                    _ => CalcMode::Programmer,
                };
                self.set_mode(mode);
                return Action::Redraw;
            }
            return Action::DragWindow;
        }

        if self.show_history && x < HISTORY_W {
            if !self.history.is_empty() && y >= self.height - 38.0 {
                self.history.clear();
                self.dirty = true;
                return Action::Redraw;
            }
            let list_y = TOOLBAR_H + 30.0;
            let row_h = 46.0;
            if y >= list_y {
                let rev_idx = ((y - list_y) / row_h).floor() as usize;
                let len = self.history.len();
                if rev_idx < len {
                    let v = self.history[len - 1 - rev_idx].value;
                    self.set_value(v);
                    self.dirty = true;
                    return Action::Redraw;
                }
            }
            return Action::None;
        }

        if self.mode == CalcMode::Programmer {
            let strip_y = TOOLBAR_H + DISPLAY_H;
            if (strip_y..strip_y + PROG_STRIP_H).contains(&y) {
                let mx = self.main_x();
                let mw = self.main_w();
                if x >= mx + mw - 84.0 && y < strip_y + 24.0 {
                    self.word_size = self.word_size.next();
                    self.set_value(self.current_val);
                    self.dirty = true;
                    return Action::Redraw;
                }
                let row = ((y - (strip_y + 4.0)) / 20.0).floor() as i32;
                let new_base = match row {
                    0 => Some(ProgBase::Hex),
                    1 => Some(ProgBase::Dec),
                    2 => Some(ProgBase::Oct),
                    3 => Some(ProgBase::Bin),
                    _ => None,
                };
                if let Some(b) = new_base {
                    self.prog_base = b;
                    self.entering = false;
                    self.dirty = true;
                    return Action::Redraw;
                }
            }
        }

        let (kx, ky, kw, kh) = self.keypad_rect();
        if x >= kx && x <= kx + kw && y >= ky && y <= ky + kh {
            let (cols, rows, specs) = self.keypad_specs();
            let gap = 7.0;
            let cell_w = (kw - gap * (cols as f32 - 1.0)) / cols as f32;
            let cell_h = (kh - gap * (rows as f32 - 1.0)) / rows as f32;
            for spec in specs {
                if !spec.enabled {
                    continue;
                }
                let bx = kx + spec.col as f32 * (cell_w + gap);
                let by = ky + spec.row as f32 * (cell_h + gap);
                let bw = cell_w * spec.col_span as f32 + gap * (spec.col_span as f32 - 1.0);
                let bh = cell_h;
                if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
                    self.press_button(spec.id);
                    return Action::Redraw;
                }
            }
        }

        Action::None
    }

    pub fn draw(&mut self, pm: &mut PixmapMut) {
        self.dirty = false;
        let w = self.width;
        let h = self.height;
        let dark = feather_ui::scale::appearance() == feather_ui::scale::Appearance::Dark;

        pm.fill(Color::TRANSPARENT);
        let bg = if dark {
            rgba(28, 29, 34, 248)
        } else {
            rgba(244, 245, 249, 250)
        };
        fill_rrect(pm, 0.0, 0.0, w, h, RADIUS, bg);

        self.draw_toolbar(pm, dark);
        if self.show_history {
            self.draw_history_sidebar(pm, dark);
        }
        self.draw_display(pm, dark);
        if self.mode == CalcMode::Programmer {
            self.draw_programmer_strip(pm, dark);
        }
        self.draw_keypad(pm, dark);

        stroke_rrect(pm, 0.5, 0.5, w - 1.0, h - 1.0, RADIUS, 1.0, colors::separator());
    }

    fn draw_toolbar(&mut self, pm: &mut PixmapMut, dark: bool) {
        let hovered_tl = hit_traffic_lights(self.hover_x, self.hover_y) != TrafficHit::None;
        let cy = TOOLBAR_H * 0.5;
        for (hit, cx) in [
            (TrafficHit::Close, 20.0),
            (TrafficHit::Minimize, 40.0),
            (TrafficHit::Maximize, 60.0),
        ] {
            let color = if !self.focused && !hovered_tl {
                colors::traffic_inactive()
            } else {
                match hit {
                    TrafficHit::Close => rgba(255, 95, 87, 255),
                    TrafficHit::Minimize => rgba(254, 188, 46, 255),
                    _ => rgba(40, 200, 64, 255),
                }
            };
            fill_circle(pm, cx, cy, 6.0, color);
            if hovered_tl {
                let ink = rgba(0, 0, 0, 140);
                match hit {
                    TrafficHit::Close => {
                        line(pm, cx - 2.5, cy - 2.5, cx + 2.5, cy + 2.5, 1.2, ink);
                        line(pm, cx + 2.5, cy - 2.5, cx - 2.5, cy + 2.5, 1.2, ink);
                    }
                    TrafficHit::Minimize => line(pm, cx - 3.0, cy, cx + 3.0, cy, 1.2, ink),
                    _ => {
                        line(pm, cx - 3.0, cy, cx + 3.0, cy, 1.2, ink);
                        line(pm, cx, cy - 3.0, cx, cy + 3.0, 1.2, ink);
                    }
                }
            }
        }

        // Segmented mode switcher (Обычный / Инженерный / Программист)
        let seg_w = 246.0_f32.min(self.width - 140.0).max(168.0);
        let seg_x = ((self.width - seg_w) * 0.5).max(74.0);
        let seg_y = 8.0;
        let seg_h = 26.0;
        let track_col = if dark {
            rgba(255, 255, 255, 18)
        } else {
            rgba(0, 0, 0, 14)
        };
        fill_rrect(pm, seg_x, seg_y, seg_w, seg_h, 7.0, track_col);

        let modes = [CalcMode::Basic, CalcMode::Scientific, CalcMode::Programmer];
        let item_w = seg_w / 3.0;
        for (i, &m) in modes.iter().enumerate() {
            let ix = seg_x + i as f32 * item_w;
            let active = self.mode == m;
            if active {
                let pill_col = if dark {
                    rgba(255, 255, 255, 38)
                } else {
                    rgba(255, 255, 255, 235)
                };
                fill_rrect(pm, ix + 2.0, seg_y + 2.0, item_w - 4.0, seg_h - 4.0, 5.5, pill_col);
            }
            let text_col = if active {
                colors::text()
            } else {
                colors::secondary()
            };
            let lbl = m.label();
            self.fonts.draw_text_centered(
                pm,
                &lbl,
                ix + item_w * 0.5,
                seg_y + 5.5,
                11.0,
                text_col,
                active,
            );
        }

        // History tape toggle button on the right
        let hist_x = self.width - 40.0;
        let hist_hover = (hist_x..=hist_x + 28.0).contains(&self.hover_x)
            && (8.0..=34.0).contains(&self.hover_y);
        if self.show_history || hist_hover {
            let pill = if self.show_history {
                colors::accent()
            } else if dark {
                rgba(255, 255, 255, 24)
            } else {
                rgba(0, 0, 0, 16)
            };
            fill_rrect(pm, hist_x, 8.0, 28.0, 26.0, 6.0, pill);
        }
        let icon_col = if self.show_history {
            Color::WHITE
        } else {
            colors::secondary()
        };
        self.fonts
            .draw_text_centered(pm, "🕘", hist_x + 14.0, 12.0, 13.0, icon_col, false);
    }

    fn draw_history_sidebar(&mut self, pm: &mut PixmapMut, dark: bool) {
        let side_bg = if dark {
            rgba(20, 21, 26, 220)
        } else {
            rgba(232, 234, 240, 220)
        };
        fill_rect(pm, 0.0, TOOLBAR_H, HISTORY_W, self.height - TOOLBAR_H, side_bg);
        line(
            pm,
            HISTORY_W,
            TOOLBAR_H,
            HISTORY_W,
            self.height,
            1.0,
            colors::separator(),
        );

        let hist_title = tr("Журнал");
        self.fonts.draw_text(
            pm,
            &hist_title,
            14.0,
            TOOLBAR_H + 8.0,
            11.5,
            colors::secondary(),
            true,
        );

        if self.history.is_empty() {
            let empty_lbl = tr("Нет записей");
            self.fonts.draw_text_centered(
                pm,
                &empty_lbl,
                HISTORY_W * 0.5,
                TOOLBAR_H + 90.0,
                12.0,
                colors::secondary(),
                false,
            );
            return;
        }

        let list_y = TOOLBAR_H + 30.0;
        let row_h = 46.0;
        let max_rows = ((self.height - list_y - 42.0) / row_h).floor().max(1.0) as usize;
        for (i, item) in self.history.iter().rev().take(max_rows).enumerate() {
            let ry = list_y + i as f32 * row_h;
            let hovered = self.hover_x < HISTORY_W && (ry..ry + row_h).contains(&self.hover_y);
            if hovered {
                let hcol = if dark {
                    rgba(255, 255, 255, 16)
                } else {
                    rgba(0, 0, 0, 10)
                };
                fill_rrect(pm, 6.0, ry, HISTORY_W - 12.0, row_h - 4.0, 6.0, hcol);
            }
            self.fonts.draw_text_clipped(
                pm,
                &item.expr,
                12.0,
                ry + 4.0,
                HISTORY_W - 24.0,
                11.0,
                colors::secondary(),
                false,
            );
            self.fonts.draw_text_clipped(
                pm,
                &format!("= {}", item.result),
                12.0,
                ry + 21.0,
                HISTORY_W - 24.0,
                13.5,
                colors::text(),
                true,
            );
        }

        let btn_y = self.height - 34.0;
        fill_rrect(
            pm,
            12.0,
            btn_y,
            HISTORY_W - 24.0,
            24.0,
            6.0,
            if dark {
                rgba(255, 255, 255, 18)
            } else {
                rgba(0, 0, 0, 12)
            },
        );
        let clear_lbl = tr("Очистить");
        self.fonts.draw_text_centered(
            pm,
            &clear_lbl,
            HISTORY_W * 0.5,
            btn_y + 4.5,
            11.5,
            colors::secondary(),
            false,
        );
    }

    fn draw_display(&mut self, pm: &mut PixmapMut, _dark: bool) {
        let mx = self.main_x();
        let mw = self.main_w();
        let y0 = TOOLBAR_H;

        let mut badge_x = mx + 16.0;
        if self.memory != 0.0 {
            self.fonts.draw_text(
                pm,
                "M",
                badge_x,
                y0 + 10.0,
                11.0,
                colors::accent(),
                true,
            );
            badge_x += 20.0;
        }
        if self.mode == CalcMode::Scientific {
            let unit = if self.radians { "RAD" } else { "DEG" };
            self.fonts.draw_text(
                pm,
                unit,
                badge_x,
                y0 + 10.0,
                11.0,
                colors::secondary(),
                true,
            );
        }

        if !self.expr_line.is_empty() {
            let ew = self.fonts.measure_text(&self.expr_line, 14.0, false);
            let ex = (mx + mw - 18.0 - ew).max(mx + 64.0);
            self.fonts.draw_text_clipped(
                pm,
                &self.expr_line,
                ex,
                y0 + 12.0,
                mw - 80.0,
                14.0,
                colors::secondary(),
                false,
            );
        }

        let text = self.display_text();
        let avail_w = (mw - 32.0).max(120.0);
        let mut font_sz = 40.0_f32;
        while font_sz > 20.0 {
            let tw = self.fonts.measure_text(&text, font_sz, true);
            if tw <= avail_w {
                break;
            }
            font_sz -= 2.0;
        }
        let tw = self.fonts.measure_text(&text, font_sz, true);
        let tx = (mx + mw - 16.0 - tw).max(mx + 16.0);
        let ty = y0 + 38.0 + (40.0 - font_sz) * 0.55;
        self.fonts.draw_text_clipped(
            pm,
            &text,
            tx,
            ty,
            avail_w,
            font_sz,
            colors::text(),
            true,
        );
    }

    fn draw_programmer_strip(&mut self, pm: &mut PixmapMut, dark: bool) {
        let mx = self.main_x();
        let mw = self.main_w();
        let y0 = TOOLBAR_H + DISPLAY_H;

        let card_bg = if dark {
            rgba(255, 255, 255, 12)
        } else {
            rgba(0, 0, 0, 8)
        };
        fill_rrect(pm, mx + 12.0, y0, mw - 24.0, PROG_STRIP_H - 6.0, 8.0, card_bg);

        let bases = [ProgBase::Hex, ProgBase::Dec, ProgBase::Oct, ProgBase::Bin];
        for (i, &b) in bases.iter().enumerate() {
            let ry = y0 + 4.0 + i as f32 * 19.5;
            let active = self.prog_base == b;
            if active {
                fill_rrect(
                    pm,
                    mx + 16.0,
                    ry,
                    4.0,
                    16.0,
                    2.0,
                    colors::accent(),
                );
            }
            let label_col = if active {
                colors::accent()
            } else {
                colors::secondary()
            };
            self.fonts
                .draw_text(pm, b.label(), mx + 26.0, ry + 1.0, 11.5, label_col, true);

            let val_str = self.format_prog_value(b);
            let val_col = if active {
                colors::text()
            } else {
                colors::secondary()
            };
            self.fonts.draw_text_clipped(
                pm,
                &val_str,
                mx + 68.0,
                ry + 1.0,
                mw - 156.0,
                11.5,
                val_col,
                active,
            );
        }

        let pill_x = mx + mw - 80.0;
        let pill_y = y0 + 5.0;
        fill_rrect(
            pm,
            pill_x,
            pill_y,
            62.0,
            18.0,
            5.0,
            if dark {
                rgba(255, 255, 255, 22)
            } else {
                rgba(0, 0, 0, 14)
            },
        );
        let ws_lbl = self.word_size.label();
        self.fonts.draw_text_centered(
            pm,
            &ws_lbl,
            pill_x + 31.0,
            pill_y + 2.5,
            10.5,
            colors::text(),
            true,
        );
    }

    fn draw_keypad(&mut self, pm: &mut PixmapMut, dark: bool) {
        let (kx, ky, kw, kh) = self.keypad_rect();
        let (cols, rows, specs) = self.keypad_specs();
        let gap = 7.0;
        let cell_w = (kw - gap * (cols as f32 - 1.0)) / cols as f32;
        let cell_h = (kh - gap * (rows as f32 - 1.0)) / rows as f32;
        let radius = (cell_h * 0.34).clamp(9.0, 18.0);

        for spec in specs {
            let bx = kx + spec.col as f32 * (cell_w + gap);
            let by = ky + spec.row as f32 * (cell_h + gap);
            let bw = cell_w * spec.col_span as f32 + gap * (spec.col_span as f32 - 1.0);
            let bh = cell_h;

            let hovered = spec.enabled
                && self.hover_x >= bx
                && self.hover_x <= bx + bw
                && self.hover_y >= by
                && self.hover_y <= by + bh;

            let (mut bg, text_col) = match spec.role {
                BtnRole::Operator => {
                    if spec.active {
                        (rgba(255, 255, 255, 250), rgba(255, 159, 10, 255))
                    } else {
                        (rgba(255, 159, 10, 255), Color::WHITE)
                    }
                }
                BtnRole::Utility => {
                    if dark {
                        (rgba(116, 118, 126, 235), Color::WHITE)
                    } else {
                        (rgba(206, 209, 218, 255), rgba(28, 30, 36, 255))
                    }
                }
                BtnRole::Digit => {
                    if dark {
                        (rgba(68, 70, 78, 240), Color::WHITE)
                    } else {
                        (rgba(255, 255, 255, 250), rgba(24, 26, 32, 255))
                    }
                }
                BtnRole::Scientific => {
                    if spec.active {
                        (colors::accent(), Color::WHITE)
                    } else if dark {
                        (rgba(48, 50, 58, 235), rgba(235, 237, 242, 255))
                    } else {
                        (rgba(226, 229, 237, 255), rgba(38, 42, 50, 255))
                    }
                }
            };

            if !spec.enabled {
                bg = if dark {
                    rgba(42, 43, 50, 120)
                } else {
                    rgba(225, 227, 234, 130)
                };
            } else if hovered {
                bg = lighten(bg, 18);
            }

            fill_rrect(pm, bx, by, bw, bh, radius, bg);
            if !dark && spec.enabled && !matches!(spec.role, BtnRole::Operator) {
                stroke_rrect(pm, bx, by, bw, bh, radius, 1.0, rgba(0, 0, 0, 18));
            }

            let fg = if !spec.enabled {
                colors::secondary()
            } else {
                text_col
            };
            let font_sz = match spec.role {
                BtnRole::Operator => 20.0,
                BtnRole::Digit => 18.0,
                BtnRole::Utility => 15.5,
                BtnRole::Scientific => 13.5,
            };
            self.fonts.draw_text_centered(
                pm,
                &spec.label,
                bx + bw * 0.5,
                by + (bh - font_sz) * 0.5 - 1.0,
                font_sz,
                fg,
                matches!(spec.role, BtnRole::Operator | BtnRole::Digit),
            );
        }
    }
}

fn lighten(c: Color, delta: u8) -> Color {
    let r = (c.red() * 255.0) as u8;
    let g = (c.green() * 255.0) as u8;
    let b = (c.blue() * 255.0) as u8;
    let a = (c.alpha() * 255.0) as u8;
    rgba(
        r.saturating_add(delta),
        g.saturating_add(delta),
        b.saturating_add(delta),
        a,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_evaluates_basic_scientific_programmer_and_renders_all_modes() {
        let mut app = App::new(false);

        // 1. Basic arithmetic with operator precedence: 12 + 3 * 4 = 24
        app.press_button(BtnId::Digit(1));
        app.press_button(BtnId::Digit(2));
        app.press_button(BtnId::Op(BinOp::Add));
        app.press_button(BtnId::Digit(3));
        app.press_button(BtnId::Op(BinOp::Mul));
        app.press_button(BtnId::Digit(4));
        app.press_button(BtnId::Equals);
        assert!((app.current_val - 24.0).abs() < 1e-9);
        assert_eq!(app.history.len(), 1);

        // Repeated '=' applies * 4 again -> 96
        app.press_button(BtnId::Equals);
        assert!((app.current_val - 96.0).abs() < 1e-9);

        // 2. Scientific mode: sin(30°) = 0.5 and 5! = 120
        app.set_mode(CalcMode::Scientific);
        app.clear_all();
        app.press_button(BtnId::Digit(3));
        app.press_button(BtnId::Digit(0));
        app.press_button(BtnId::Sin);
        assert!((app.current_val - 0.5).abs() < 1e-9);

        app.clear_all();
        app.press_button(BtnId::Digit(5));
        app.press_button(BtnId::Fact);
        assert!((app.current_val - 120.0).abs() < 1e-9);

        // 3. Programmer mode: 0xFF AND 0x0F = 0x0F (15), then << 4 = 240
        app.set_mode(CalcMode::Programmer);
        app.prog_base = ProgBase::Hex;
        app.clear_all();
        app.press_button(BtnId::Digit(15));
        app.press_button(BtnId::Digit(15));
        app.press_button(BtnId::Op(BinOp::BitAnd));
        app.press_button(BtnId::Digit(15));
        app.press_button(BtnId::Equals);
        assert_eq!(app.current_val as i64, 0x0F);
        app.press_button(BtnId::Op(BinOp::Shl));
        app.press_button(BtnId::Digit(4));
        app.press_button(BtnId::Equals);
        assert_eq!(app.current_val as i64, 0xF0);
        assert_eq!(app.format_prog_value(ProgBase::Hex), "F0");
        assert_eq!(app.format_prog_value(ProgBase::Bin), "1111 0000");

        // 4. Render all 3 modes + history sidebar without panicking
        app.show_history = true;
        app.set_size(640, 520);
        let mut pm = tiny_skia::Pixmap::new(640, 520).unwrap();
        for mode in [CalcMode::Basic, CalcMode::Scientific, CalcMode::Programmer] {
            app.set_mode(mode);
            app.draw(&mut pm.as_mut());
        }
    }
}
