//! Drawing the cell grid in physical pixels: colours, glyphs, procedural box drawing,
//! cursor and selection. Only rows marked dirty are repainted into a persistent canvas.

use feather_ui::text::{CellMetrics, FontSystem};
use tiny_skia::PixmapMut;

use crate::vt::term::{flags, Color, ColorKind, CursorShape, Row, Term};

pub type Rgb = [u8; 3];

#[derive(Clone)]
pub struct Palette {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub ansi: [Rgb; 16],
    pub dark: bool,
}

impl Palette {
    /// Light like Terminal.app's "Basic"
    pub fn light() -> Self {
        Palette {
            fg: [28, 28, 30],
            bg: [255, 255, 255],
            cursor: [120, 120, 128],
            selection: [178, 215, 255],
            ansi: [
                [0, 0, 0], [194, 54, 33], [37, 158, 36], [160, 140, 20], [42, 90, 210], [176, 56, 176], [30, 150, 168], [190, 190, 196],
                [104, 104, 110], [235, 70, 50], [40, 190, 40], [190, 170, 20], [70, 120, 245], [215, 80, 215], [30, 185, 200], [255, 255, 255],
            ],
            dark: false,
        }
    }

    /// Dark, close to the Tahoe dark appearance
    pub fn dark() -> Self {
        Palette {
            fg: [228, 228, 232],
            bg: [28, 28, 32],
            cursor: [200, 200, 206],
            selection: [52, 86, 140],
            ansi: [
                [40, 40, 44], [255, 95, 87], [80, 210, 100], [230, 200, 80], [90, 150, 255], [215, 120, 230], [80, 205, 220], [200, 200, 206],
                [110, 110, 118], [255, 125, 115], [110, 235, 125], [250, 220, 110], [125, 175, 255], [235, 150, 245], [115, 225, 240], [255, 255, 255],
            ],
            dark: true,
        }
    }

    pub fn color(&self, c: Color, foreground: bool, bold: bool) -> Rgb {
        match c.kind() {
            ColorKind::Default => if foreground { self.fg } else { self.bg },
            // "Bold is bright", as Terminal.app and Konsole do by default
            ColorKind::Indexed(i) if i < 8 && bold && foreground => self.ansi[i as usize + 8],
            ColorKind::Indexed(i) if i < 16 => self.ansi[i as usize],
            ColorKind::Indexed(i) if i < 232 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                [level(i / 36), level((i / 6) % 6), level(i % 6)]
            }
            ColorKind::Indexed(i) => {
                let g = 8 + (i - 232) * 10;
                [g, g, g]
            }
            ColorKind::Rgb(r, g, b) => [r, g, b],
        }
    }
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

/// Opaque rectangle in physical pixels, clipped to the pixmap
pub fn fill(pm: &mut PixmapMut, x: i32, y: i32, w: i32, h: i32, rgb: Rgb) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let (x0, y0, x1, y1) = (x.max(0), y.max(0), (x + w).min(pw), (y + h).min(ph));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let pixel = [rgb[0], rgb[1], rgb[2], 255];
    let data = pm.data_mut();
    for row in y0..y1 {
        let start = ((row * pw + x0) * 4) as usize;
        for px in data[start..start + ((x1 - x0) * 4) as usize].chunks_exact_mut(4) {
            px.copy_from_slice(&pixel);
        }
    }
}

/// Blends `rgb` at `alpha` (0–255) over a rectangle (shade blocks, selection over colour)
fn blend(pm: &mut PixmapMut, x: i32, y: i32, w: i32, h: i32, rgb: Rgb, alpha: u32) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let (x0, y0, x1, y1) = (x.max(0), y.max(0), (x + w).min(pw), (y + h).min(ph));
    let data = pm.data_mut();
    for row in y0..y1 {
        for col in x0..x1 {
            let i = ((row * pw + col) * 4) as usize;
            for c in 0..3 {
                data[i + c] = ((rgb[c] as u32 * alpha + data[i + c] as u32 * (255 - alpha)) / 255) as u8;
            }
        }
    }
}

/// Box drawing and block elements drawn on the cell grid, so lines join without gaps
/// whatever the font's line height. Returns false for other characters.
fn draw_box(pm: &mut PixmapMut, c: char, x: i32, y: i32, w: i32, h: i32, rgb: Rgb, scale: f32) -> bool {
    let light = (scale.round() as i32).max(1);
    let heavy = light * 2;
    let (cx, cy) = (x + (w - light) / 2, y + (h - light) / 2);
    // Arms: (left, right, up, down) thickness; 0 = none; negative = double line
    let arms: Option<[i32; 4]> = match c {
        '─' => Some([1, 1, 0, 0]),
        '━' => Some([2, 2, 0, 0]),
        '│' => Some([0, 0, 1, 1]),
        '┃' => Some([0, 0, 2, 2]),
        '┌' | '╭' => Some([0, 1, 0, 1]),
        '┐' | '╮' => Some([1, 0, 0, 1]),
        '└' | '╰' => Some([0, 1, 1, 0]),
        '┘' | '╯' => Some([1, 0, 1, 0]),
        '├' => Some([0, 1, 1, 1]),
        '┤' => Some([1, 0, 1, 1]),
        '┬' => Some([1, 1, 0, 1]),
        '┴' => Some([1, 1, 1, 0]),
        '┼' => Some([1, 1, 1, 1]),
        '┏' => Some([0, 2, 0, 2]),
        '┓' => Some([2, 0, 0, 2]),
        '┗' => Some([0, 2, 2, 0]),
        '┛' => Some([2, 0, 2, 0]),
        '┣' => Some([0, 2, 2, 2]),
        '┫' => Some([2, 0, 2, 2]),
        '┳' => Some([2, 2, 0, 2]),
        '┻' => Some([2, 2, 2, 0]),
        '╋' => Some([2, 2, 2, 2]),
        '═' => Some([-1, -1, 0, 0]),
        '║' => Some([0, 0, -1, -1]),
        '╔' => Some([0, -1, 0, -1]),
        '╗' => Some([-1, 0, 0, -1]),
        '╚' => Some([0, -1, -1, 0]),
        '╝' => Some([-1, 0, -1, 0]),
        '╠' => Some([0, -1, -1, -1]),
        '╣' => Some([-1, 0, -1, -1]),
        '╦' => Some([-1, -1, 0, -1]),
        '╩' => Some([-1, -1, -1, 0]),
        '╬' => Some([-1, -1, -1, -1]),
        _ => None,
    };
    if let Some([l, r, u, d]) = arms {
        let double = [l, r, u, d].contains(&-1);
        if double {
            // Two thin lines `g` from the centre. Where a perpendicular arm leaves on a
            // line's side, that line stops short of the centre (inner corner), otherwise
            // it runs past it (outer corner): ╔ ╦ ╠ ╬ join like real double lines.
            let g = light;
            let (hv, vv) = (u != 0 || d != 0, l != 0 || r != 0);
            for o in [-g, g] {
                let hy = cy + o;
                // Horizontal arms: does a vertical arm leave on this line's side?
                let side_v = if o < 0 { u != 0 } else { d != 0 };
                if l != 0 {
                    let end = if side_v { cx - g + light } else if hv { cx + g + light } else { cx + light };
                    fill(pm, x, hy, end - x, light, rgb)
                }
                if r != 0 {
                    let start = if side_v { cx + g } else if hv { cx - g } else { cx };
                    fill(pm, start, hy, x + w - start, light, rgb)
                }
                let vx = cx + o;
                let side_h = if o < 0 { l != 0 } else { r != 0 };
                if u != 0 {
                    let end = if side_h { cy - g + light } else if vv { cy + g + light } else { cy + light };
                    fill(pm, vx, y, light, end - y, rgb)
                }
                if d != 0 {
                    let start = if side_h { cy + g } else if vv { cy - g } else { cy };
                    fill(pm, vx, start, light, y + h - start, rgb)
                }
            }
            return true;
        }
        let t = |k: i32| if k == 2 { heavy } else { light };
        if l != 0 { fill(pm, x, cy - (t(l) - light) / 2, cx - x + t(l), t(l), rgb) }
        if r != 0 { fill(pm, cx, cy - (t(r) - light) / 2, x + w - cx, t(r), rgb) }
        if u != 0 { fill(pm, cx - (t(u) - light) / 2, y, t(u), cy - y + t(u), rgb) }
        if d != 0 { fill(pm, cx - (t(d) - light) / 2, cy, t(d), y + h - cy, rgb) }
        return true;
    }
    match c {
        '█' => fill(pm, x, y, w, h, rgb),
        '▀' => fill(pm, x, y, w, h / 2, rgb),
        '▄' => fill(pm, x, y + h / 2, w, h - h / 2, rgb),
        '▌' => fill(pm, x, y, w / 2, h, rgb),
        '▐' => fill(pm, x + w / 2, y, w - w / 2, h, rgb),
        '▁' => fill(pm, x, y + h - h / 8, w, h / 8, rgb),
        '▂' => fill(pm, x, y + h - h / 4, w, h / 4, rgb),
        '▃' => fill(pm, x, y + h - 3 * h / 8, w, 3 * h / 8, rgb),
        '▅' => fill(pm, x, y + h - 5 * h / 8, w, 5 * h / 8, rgb),
        '▆' => fill(pm, x, y + h - 3 * h / 4, w, 3 * h / 4, rgb),
        '▇' => fill(pm, x, y + h - 7 * h / 8, w, 7 * h / 8, rgb),
        '░' => blend(pm, x, y, w, h, rgb, 64),
        '▒' => blend(pm, x, y, w, h, rgb, 128),
        '▓' => blend(pm, x, y, w, h, rgb, 192),
        _ => return false,
    }
    true
}

/// Inclusive cell range of the selection in absolute line numbers
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SelRange {
    pub start: (i64, usize),
    pub end: (i64, usize),
}

impl SelRange {
    pub fn contains(&self, abs: i64, col: usize) -> bool {
        (abs, col) >= self.start && (abs, col) <= self.end
    }
}

pub struct Grid {
    pub fonts: FontSystem,
    pub size_pt: f32,
    pub metrics: CellMetrics,
    phys_size: f32,
    scale: f32,
}

pub struct Frame<'a> {
    pub term: &'a Term,
    pub offset: usize,
    pub palette: &'a Palette,
    pub selection: Option<SelRange>,
    pub focused: bool,
    /// Physical top-left of the cell grid
    pub origin: (i32, i32),
    pub cursor_on: bool,
    /// Search matches on screen: (abs line, first col, last col)
    pub matches: &'a [(i64, usize, usize)],
    pub active_match: Option<SelRange>,
}

impl Grid {
    pub fn new(size_pt: f32, scale: f32) -> Self {
        let mut fonts = FontSystem::new_monospace();
        let phys_size = size_pt * scale;
        let metrics = fonts.cell_metrics(phys_size);
        Grid { fonts, size_pt, metrics, phys_size, scale }
    }

    pub fn set(&mut self, size_pt: f32, scale: f32) {
        if (size_pt, scale) != (self.size_pt, self.scale) {
            self.size_pt = size_pt;
            self.scale = scale;
            self.phys_size = size_pt * scale;
            self.metrics = self.fonts.cell_metrics(self.phys_size);
        }
    }

    pub fn cell_px(&self) -> (i32, i32) {
        (self.metrics.width as i32, self.metrics.height as i32)
    }

    /// Repaints display row `y` (full width, padding included)
    pub fn draw_row(&mut self, pm: &mut PixmapMut, f: &Frame, y: usize) {
        let (cw, ch) = self.cell_px();
        let term = f.term;
        let top = f.origin.1 + y as i32 * ch;
        let p = f.palette;
        let reverse = term.modes.reverse_video;
        let base_bg = if reverse { p.fg } else { p.bg };
        fill(pm, 0, top, pm.width() as i32, ch, base_bg);
        let row: &Row = term.view_row(f.offset, y);
        let abs = term.scrolled_out as i64 - f.offset as i64 + y as i64;
        let cursor_here = f.offset == 0 && y == term.y && term.modes.cursor_visible && f.cursor_on;
        let baseline = top + self.metrics.ascent as i32;
        let cols = term.cols();
        let first_match=f.matches.partition_point(|&(line,_,_)|line<abs);
        let row_matches=&f.matches[first_match..first_match+f.matches[first_match..].partition_point(|&(line,_,_)|line==abs)];
        let mut match_index=0;
        for x in 0..cols {
            let cell = row.cells.get(x).copied().unwrap_or_default();
            if cell.flags & flags::SPACER != 0 {
                continue;
            }
            let width = if cell.flags & flags::WIDE != 0 { 2 } else { 1 };
            let left = f.origin.0 + x as i32 * cw;
            let bold = cell.flags & flags::BOLD != 0;
            let mut fg = p.color(cell.fg, true, bold);
            let mut bg = p.color(cell.bg, false, false);
            if (cell.flags & flags::INVERSE != 0) != reverse {
                std::mem::swap(&mut fg, &mut bg);
                if cell.fg == Color::DEFAULT && cell.bg == Color::DEFAULT && !reverse {
                    fg = p.bg;
                    bg = p.fg;
                }
            }
            if cell.flags & flags::DIM != 0 {
                fg = mix(fg, bg, 0.45);
            }
            let selected = f.selection.is_some_and(|s| s.contains(abs, x));
            while match_index<row_matches.len() && row_matches[match_index].2<x {match_index+=1;}
            let matched=row_matches.get(match_index).is_some_and(|&(_,a,b)|x>=a&&x<=b);
            if selected {
                bg = p.selection;
                if !p.dark {
                    fg = mix(fg, [0, 0, 0], 0.1);
                }
            } else if matched {
                bg = if f.active_match.is_some_and(|s|s.contains(abs,x)) {[255,159,10]} else {[255,214,10]};
                fg = [20, 20, 20];
            }
            let is_cursor = cursor_here && x == term.x;
            if is_cursor && f.focused && term.cursor_shape == CursorShape::Block {
                bg = p.cursor;
                fg = p.bg;
            }
            if bg != base_bg {
                fill(pm, left, top, cw * width, ch, bg);
            }
            let hidden = cell.flags & flags::HIDDEN != 0;
            if !hidden && cell.ch != ' ' && !draw_box(pm, cell.ch, left, top, cw * width, ch, fg, self.scale) {
                self.fonts.draw_glyph(pm, cell.ch, left, baseline, self.phys_size, fg, bold);
            }
            if !hidden {
                for (col, marks) in &row.combining {
                    if *col as usize == x {
                        // Marks are zero-width and positioned after the base glyph's advance
                        for m in marks.chars() {
                            self.fonts.draw_glyph(pm, m, left + cw * width, baseline, self.phys_size, fg, bold);
                        }
                    }
                }
            }
            let line = (self.scale.round() as i32).max(1);
            if cell.flags & (flags::UNDERLINE | flags::DOUBLE_UNDERLINE) != 0 {
                let uy = baseline + line * 2;
                fill(pm, left, uy, cw * width, line, fg);
                if cell.flags & flags::DOUBLE_UNDERLINE != 0 {
                    fill(pm, left, uy + line * 2, cw * width, line, fg);
                }
            }
            if cell.flags & flags::STRIKE != 0 {
                fill(pm, left, top + ch / 2, cw * width, line, fg);
            }
            if is_cursor && !(f.focused && term.cursor_shape == CursorShape::Block) {
                let thick = (self.scale * 2.0).round() as i32;
                match (f.focused, term.cursor_shape) {
                    (true, CursorShape::Bar) => fill(pm, left, top, thick, ch, p.cursor),
                    (true, CursorShape::Underline) => fill(pm, left, top + ch - thick, cw * width, thick, p.cursor),
                    _ => {
                        // Hollow block when the window is not focused
                        let l = line;
                        fill(pm, left, top, cw * width, l, p.cursor);
                        fill(pm, left, top + ch - l, cw * width, l, p.cursor);
                        fill(pm, left, top, l, ch, p.cursor);
                        fill(pm, left + cw * width - l, top, l, ch, p.cursor);
                    }
                }
            }
        }
    }
}

/// Cell under a physical point (clamped to the grid)
pub fn cell_at(px: f32, py: f32, origin: (i32, i32), cell: (i32, i32), cols: usize, rows: usize) -> (usize, usize) {
    let x = ((px - origin.0 as f32) / cell.0 as f32).floor().clamp(0.0, (cols - 1) as f32) as usize;
    let y = ((py - origin.1 as f32) / cell.1 as f32).floor().clamp(0.0, (rows - 1) as f32) as usize;
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_256_and_bold_bright() {
        let p = Palette::light();
        assert_eq!(p.color(Color::indexed(1), true, true), p.ansi[9]);
        assert_eq!(p.color(Color::indexed(1), false, true), p.ansi[1]);
        assert_eq!(p.color(Color::indexed(16), true, false), [0, 0, 0]);
        assert_eq!(p.color(Color::indexed(231), true, false), [255, 255, 255]);
        assert_eq!(p.color(Color::indexed(196), true, false), [255, 0, 0]);
        assert_eq!(p.color(Color::indexed(232), true, false), [8, 8, 8]);
        assert_eq!(p.color(Color::rgb(1, 2, 3), false, false), [1, 2, 3]);
    }

    #[test]
    fn box_lines_join_across_cells() {
        let mut pix = tiny_skia::Pixmap::new(30, 20).unwrap();
        let mut pm = pix.as_mut();
        assert!(draw_box(&mut pm, '─', 0, 0, 10, 20, [255, 0, 0], 1.0));
        assert!(draw_box(&mut pm, '┼', 10, 0, 10, 20, [255, 0, 0], 1.0));
        assert!(draw_box(&mut pm, '─', 20, 0, 10, 20, [255, 0, 0], 1.0));
        assert!(!draw_box(&mut pm, 'a', 0, 0, 10, 20, [255, 0, 0], 1.0));
        let red = |x: u32, y: u32| pix.pixel(x, y).unwrap().red() == 255;
        // One continuous horizontal line through all three cells, and the vertical bar
        let row = (0..20).find(|&y| red(0, y)).unwrap();
        assert!((0..30).all(|x| red(x, row)));
        assert!(red(14, 0) || red(15, 0));
        assert!(red(14, 19) || red(15, 19));
    }

    #[test]
    fn search_highlights_distinguish_active_and_other_hits() {
        let term=Term::new(6,2);let mut grid=Grid::new(12.0,1.0);let (cw,ch)=grid.cell_px();
        let mut pix=tiny_skia::Pixmap::new((cw*6) as u32,(ch*2) as u32).unwrap();let palette=Palette::light();
        let frame=Frame{term:&term,offset:0,palette:&palette,selection:None,focused:false,origin:(0,0),cursor_on:false,matches:&[(0,1,1),(0,3,3)],active_match:Some(SelRange{start:(0,3),end:(0,3)})};
        grid.draw_row(&mut pix.as_mut(),&frame,0);
        let color=|x:i32|{let p=pix.pixel((x*cw+1) as u32,1).unwrap();[p.red(),p.green(),p.blue()]};
        assert_eq!(color(1),[255,214,10]);assert_eq!(color(3),[255,159,10]);assert_eq!(color(2),palette.bg);
    }

    #[test]
    fn rows_paint_colors_selection_and_cursor() {
        let mut term = Term::new(10, 2);
        crate::vt::parser::Parser::new().advance(&mut term, b"\x1b[41mA\x1b[0m B");
        let mut grid = Grid::new(12.0, 1.0);
        let (cw, ch) = grid.cell_px();
        let mut pix = tiny_skia::Pixmap::new((cw * 10 + 10) as u32, (ch * 2 + 10) as u32).unwrap();
        let p = Palette::light();
        let sel = SelRange { start: (0, 2), end: (0, 2) };
        let frame = Frame { term: &term, offset: 0, palette: &p, selection: Some(sel), focused: true, origin: (5, 5), cursor_on: true, matches: &[], active_match: None };
        grid.draw_row(&mut pix.as_mut(), &frame, 0);
        let at = |x: i32, y: i32| { let c = pix.pixel(x as u32, y as u32).unwrap(); [c.red(), c.green(), c.blue()] };
        assert_eq!(at(5 + 1, 5 + 1), p.ansi[1], "red background of A");
        assert_eq!(at(5 + cw * 2 + 1, 5 + 1), p.selection, "selected B");
        assert_eq!(at(5 + cw * 3 + 1, 5 + 1), p.cursor, "block cursor after B");
        assert_eq!(at(1, 5 + 1), p.bg, "left padding of the row has the background");
    }
}
