//! Literal, Unicode-aware search. History is borrowed in bounded event-loop slices;
//! no history snapshot or list of all matches is kept in memory.
use crate::{
    view::SelRange,
    vt::term::{flags, Term},
};
use std::collections::VecDeque;

const MAX_QUERY: usize = 256;
type Pos = (i64, usize);

#[derive(Default, Clone)]
struct Pattern {
    chars: Vec<char>,
    prefix: Vec<usize>,
}
impl Pattern {
    fn new(query: &str) -> Self {
        let chars: Vec<_> = query.chars().flat_map(char::to_lowercase).collect();
        let mut prefix = vec![0; chars.len()];
        for i in 1..chars.len() {
            let mut j = prefix[i - 1];
            while j > 0 && chars[i] != chars[j] {
                j = prefix[j - 1];
            }
            if chars[i] == chars[j] {
                j += 1;
            }
            prefix[i] = j;
        }
        Self { chars, prefix }
    }
}

struct Scanner {
    pattern: Pattern,
    line: i64,
    end: i64,
    col: usize,
    row_len: Option<usize>,
    matched: usize,
    positions: VecDeque<Pos>,
}
impl Scanner {
    fn new(pattern: Pattern, line: i64, end: i64) -> Self {
        Self {
            pattern,
            line,
            end,
            col: 0,
            row_len: None,
            matched: 0,
            positions: VecDeque::new(),
        }
    }
    fn feed(&mut self, ch: char, start: Pos, end: Pos, found: &mut impl FnMut(SelRange)) {
        for c in ch.to_lowercase() {
            self.positions.push_back(start);
            if self.positions.len() > self.pattern.chars.len() {
                self.positions.pop_front();
            }
            while self.matched > 0 && c != self.pattern.chars[self.matched] {
                self.matched = self.pattern.prefix[self.matched - 1];
            }
            if c == self.pattern.chars[self.matched] {
                self.matched += 1;
            }
            if self.matched == self.pattern.chars.len() {
                found(SelRange {
                    start: *self.positions.front().unwrap(),
                    end,
                });
                self.matched = self.pattern.prefix[self.matched - 1];
            }
        }
    }
    fn step(&mut self, term: &Term, budget: usize, mut found: impl FnMut(SelRange)) -> bool {
        if self.pattern.chars.is_empty() {
            return true;
        }
        let mut work = 0;
        while self.line < self.end && work < budget {
            let Some(row) = term.abs_row(self.line) else {
                self.line += 1;
                self.col = 0;
                self.row_len = None;
                continue;
            };
            let len = *self.row_len.get_or_insert_with(|| {
                if row.wrapped {
                    row.cells.len()
                } else {
                    row.cells
                        .iter()
                        .rposition(|c| c.ch != ' ')
                        .map_or(0, |i| i + 1)
                }
            });
            if self.col >= len {
                if !row.wrapped {
                    self.matched = 0;
                    self.positions.clear();
                }
                self.line += 1;
                self.col = 0;
                self.row_len = None;
                work += 1;
                continue;
            }
            let col = self.col;
            let cell = row.cells[col];
            self.col += 1;
            work += 1;
            if cell.flags & flags::SPACER != 0 {
                continue;
            }
            let start = (self.line, col);
            let end = (self.line, col + usize::from(cell.flags & flags::WIDE != 0));
            self.feed(cell.ch, start, end, &mut found);
            if let Some((_, marks)) = row.combining.iter().find(|(x, _)| *x as usize == col) {
                for c in marks.chars() {
                    self.feed(c, start, end, &mut found);
                }
            }
        }
        self.line >= self.end
    }
}

enum Target {
    Near(Pos),
    Index(usize),
}
struct Job {
    scanner: Scanner,
    target: Target,
    count: usize,
    chosen: Option<(usize, SelRange)>,
    last: Option<(usize, SelRange)>,
}

#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub query: String,
    pub caret: usize,
    pub select_all: bool,
    pub count: usize,
    pub current: Option<SelRange>,
    pub index: usize,
    pattern: Pattern,
    job: Option<Job>,
}

impl Search {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
    pub fn set_query(&mut self, text: &str) {
        self.query = text
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_QUERY)
            .collect();
        self.caret = self.query.len();
        self.select_all = false;
    }
    pub fn insert(&mut self, text: &str) {
        if self.select_all {
            self.query.clear();
            self.caret = 0;
            self.select_all = false;
        }
        let room = MAX_QUERY.saturating_sub(self.query.chars().count());
        let text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(room)
            .collect();
        self.query.insert_str(self.caret, &text);
        self.caret += text.len();
    }
    pub fn backspace(&mut self, delete: bool) {
        if self.select_all {
            self.query.clear();
            self.caret = 0;
            self.select_all = false;
            return;
        }
        if delete {
            if let Some(c) = self.query[self.caret..].chars().next() {
                self.query.drain(self.caret..self.caret + c.len_utf8());
            }
        } else if let Some((i, _)) = self.query[..self.caret].char_indices().next_back() {
            self.query.drain(i..self.caret);
            self.caret = i;
        }
    }
    pub fn move_caret(&mut self, right: bool) {
        self.select_all = false;
        self.caret = if right {
            self.query[self.caret..]
                .chars()
                .next()
                .map_or(self.caret, |c| self.caret + c.len_utf8())
        } else {
            self.query[..self.caret]
                .char_indices()
                .next_back()
                .map_or(0, |(i, _)| i)
        };
    }
    fn start(&mut self, term: &Term, target: Target) {
        self.pattern = Pattern::new(&self.query);
        self.current = None;
        if self.pattern.chars.is_empty() {
            self.job = None;
            self.count = 0;
            return;
        }
        let first = if term.alt_active {
            term.scrolled_out as i64
        } else {
            term.first_abs()
        };
        self.job = Some(Job {
            scanner: Scanner::new(
                self.pattern.clone(),
                first,
                term.scrolled_out as i64 + term.rows() as i64,
            ),
            target,
            count: 0,
            chosen: None,
            last: None,
        });
    }
    pub fn restart(&mut self, term: &Term, anchor: Pos) {
        self.start(term, Target::Near(anchor));
    }
    pub fn navigate(&mut self, term: &Term, previous: bool) {
        if self.count == 0 || self.busy() {
            return;
        }
        let index = if previous {
            (self.index + self.count - 1) % self.count
        } else {
            (self.index + 1) % self.count
        };
        self.start(term, Target::Index(index));
    }
    /// At most `budget` grid cells, regardless of history length.
    pub fn step(&mut self, term: &Term, budget: usize) -> bool {
        let Some(job) = self.job.as_mut() else {
            return true;
        };
        let done = job.scanner.step(term, budget, |hit| {
            let index = job.count;
            job.count += 1;
            job.last = Some((index, hit));
            if job.chosen.is_none()
                && match job.target {
                    Target::Near(p) => hit.end >= p,
                    Target::Index(i) => i == index,
                }
            {
                job.chosen = Some((index, hit));
            }
        });
        if done {
            let job = self.job.take().unwrap();
            self.count = job.count;
            if let Some((i, hit)) = job.chosen.or(job.last) {
                self.index = i;
                self.current = Some(hit);
            } else {
                self.index = 0;
                self.current = None;
            }
        }
        done
    }
    pub fn close(&mut self) {
        self.open = false;
        self.job = None;
        self.current = None;
    }

    pub fn visible_matches(&self, term: &Term, offset: usize) -> Vec<(i64, usize, usize)> {
        if !self.open || self.pattern.chars.is_empty() {
            return Vec::new();
        }
        let top = term.scrolled_out as i64 - offset as i64;
        let bottom = top + term.rows() as i64;
        let first = if term.alt_active {
            term.scrolled_out as i64
        } else {
            term.first_abs()
        };
        let mut start = top;
        let mut context = self.pattern.chars.len() * 2;
        while start > first && context > 0 {
            let row = term.abs_row(start - 1).unwrap();
            if !row.wrapped {
                break;
            }
            start -= 1;
            context = context.saturating_sub(row.cells.len().max(1));
        }
        let mut end = bottom;
        let mut context = self.pattern.chars.len() * 2;
        while end < term.scrolled_out as i64 + term.rows() as i64 && context > 0 {
            if !term.abs_row(end - 1).is_some_and(|r| r.wrapped) {
                break;
            }
            let row = term.abs_row(end).unwrap();
            end += 1;
            context = context.saturating_sub(row.cells.len().max(1));
        }
        let mut result = Vec::new();
        let mut scanner = Scanner::new(self.pattern.clone(), start, end);
        scanner.step(term, usize::MAX, |hit| {
            for line in hit.start.0.max(top)..=hit.end.0.min(bottom - 1) {
                let a = if line == hit.start.0 { hit.start.1 } else { 0 };
                let b = if line == hit.end.0 {
                    hit.end.1
                } else {
                    term.cols() - 1
                };
                result.push((line, a, b));
            }
        });
        result.sort_unstable();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vt::parser::Parser;
    fn run(search: &mut Search, term: &Term) {
        while !search.step(term, 3) {}
    }
    #[test]
    fn history_wrapped_unicode_and_navigation_without_storing_all_hits() {
        let mut t = Term::new(8, 3);
        Parser::new().advance(&mut t, "one\r\nПривет界a\u{301}\r\nlast\r\none".as_bytes());
        let mut s = Search {
            open: true,
            ..Default::default()
        };
        s.set_query("ONE");
        s.restart(&t, (t.first_abs(), 0));
        run(&mut s, &t);
        assert_eq!(s.count, 2);
        let first = s.current;
        s.navigate(&t, false);
        run(&mut s, &t);
        assert_ne!(first, s.current);
        s.navigate(&t, false);
        run(&mut s, &t);
        assert_eq!(first, s.current);
        s.navigate(&t, true);
        run(&mut s, &t);
        assert_eq!(s.index, 1);
        s.set_query("ВЕТ界a\u{301}");
        s.restart(&t, (t.first_abs(), 0));
        run(&mut s, &t);
        assert_eq!(s.count, 1);
        let hit = s.current.unwrap();
        assert!(hit.end.0 > hit.start.0);
        assert!(!s.visible_matches(&t, t.scrollback_len()).is_empty());
    }
    #[test]
    fn hard_newlines_do_not_match_and_alternate_screen_excludes_main_history() {
        let mut t = Term::new(20, 2);
        Parser::new().advance(&mut t, b"abc\r\ndef\r\nabc");
        let mut s = Search::default();
        s.set_query("abcdef");
        s.restart(&t, (0, 0));
        run(&mut s, &t);
        assert_eq!(s.count, 0);
        Parser::new().advance(&mut t, b"\x1b[?1049hother");
        s.set_query("abc");
        s.restart(&t, (0, 0));
        run(&mut s, &t);
        assert_eq!(s.count, 0);
    }
    #[test]
    fn unicode_editing_and_scan_cancellation() {
        let mut s = Search::default();
        s.insert("a界б");
        s.move_caret(false);
        s.backspace(false);
        assert_eq!(s.query, "aб");
        s.select_all = true;
        s.insert("test");
        assert_eq!(s.query, "test");
        let t = Term::new(80, 24);
        s.restart(&t, (0, 0));
        assert!(!s.step(&t, 1));
        s.set_query("");
        s.restart(&t, (0, 0));
        assert!(!s.busy());
        assert_eq!(s.count, 0);
    }
}
