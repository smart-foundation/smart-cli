//! The presenter framework: the CLI's rendering vocabulary, ported from the
//! shell's presentation layer (the shell's `widgets.rs` / `color.rs` — read
//! only; the approach is copied, that tree is not edited).
//!
//! # What is carried over, and why
//!
//! * **The Cell split.** A cell is `{content, style}`: the text the terminal
//!   draws, and the style it wears. The style is applied AFTER the column
//!   widths are known, so a width is never measured over escape sequences —
//!   the mistake that makes a styled cell guess.
//! * **The width measure.** A width is the number of terminal COLUMNS the
//!   text occupies: East-Asian Wide/Fullwidth glyphs two, combining marks
//!   zero, everything else one. `◉` is one column in a Western terminal, so
//!   a table that draws `◉ active` lines up with the ASCII beside it.
//! * **The width-aware cut.** A cell that outgrows its column is cut in the
//!   MIDDLE, with the cut marked (`…`) and both ends surviving: the head says
//!   what it is, the tail says which one it is. A path keeps its basename.
//! * **The box tables.** `┌─┬─┐ │ ├─┼─┤ │ └─┴─┘` with the columns aligned
//!   on their measured widths.
//! * **The glyph vocabulary.** `◉` active, `○` unknown, `●` on, `◌` off,
//!   `◐` half — one glyph per state, never a colour alone.
//!
//! The core stays platform-agnostic: no POSIX identity is required in any
//! position, and the locale only decides how AMBIGUOUS glyphs are measured.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::render::Style as Paint;

/// The style a cell wears when the table prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sgr {
    /// No style.
    #[default]
    Plain,
    /// The "this is fine" colour.
    Green,
    /// The "look at this" colour.
    Yellow,
    /// The "this failed" colour.
    Red,
    /// The "this is information" colour.
    Cyan,
    /// Emphasis.
    Bold,
    /// De-emphasis.
    Dimmed,
}

impl Sgr {
    /// The sequence that opens a styled run.
    pub fn open(self) -> &'static str {
        match self {
            Sgr::Plain => "",
            Sgr::Red => "\u{1b}[31m",
            Sgr::Green => "\u{1b}[32m",
            Sgr::Yellow => "\u{1b}[33m",
            Sgr::Cyan => "\u{1b}[36m",
            Sgr::Bold => "\u{1b}[1m",
            Sgr::Dimmed => "\u{1b}[2m",
        }
    }

    /// The sequence that ends a styled run.
    pub const RESET: &'static str = "\u{1b}[0m";
}

/// How a cell is cut when it outgrows its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellShape {
    /// Ordinary text: the middle is cut, and the cut is marked.
    #[default]
    Text,
    /// A filesystem path: the head keeps its leading directories and the
    /// BASENAME survives whole — the tail is the identity.
    Path,
}

/// One table cell: the text the terminal draws, and the style it wears.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    content: String,
    style: Sgr,
    shape: CellShape,
}

impl Cell {
    /// A cell with no style.
    pub fn plain(content: impl Into<String>) -> Cell {
        Cell {
            content: content.into(),
            style: Sgr::Plain,
            shape: CellShape::Text,
        }
    }

    /// A cell that prints its content in `style`. The content stays plain
    /// text: the colour is the table's business, applied at render time.
    pub fn styled(content: impl Into<String>, style: Sgr) -> Cell {
        Cell {
            content: content.into(),
            style,
            shape: CellShape::Text,
        }
    }

    /// A cell holding a filesystem path.
    pub fn path(content: impl Into<String>) -> Cell {
        Cell {
            content: content.into(),
            style: Sgr::Plain,
            shape: CellShape::Path,
        }
    }

    /// The text the terminal draws — what every width is measured on.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// The style the cell wears.
    pub fn style(&self) -> Sgr {
        self.style
    }

    /// How this cell is cut when it outgrows its column.
    pub fn shape(&self) -> CellShape {
        self.shape
    }

    /// The cell's text, cut to `width` and marked when it had to be cut.
    pub fn cut(&self, width: usize) -> String {
        match self.shape {
            CellShape::Path => truncate_path(&self.content, width),
            CellShape::Text => truncate_middle(&self.content, width),
        }
    }
}

impl From<String> for Cell {
    fn from(content: String) -> Cell {
        Cell::plain(content)
    }
}

impl From<&str> for Cell {
    fn from(content: &str) -> Cell {
        Cell::plain(content)
    }
}

/// The state glyphs: one glyph per state, never a colour alone.
pub mod glyph {
    /// The state is active / connected.
    pub const ACTIVE: &str = "◉";
    /// The state is unknown / not applicable.
    pub const UNKNOWN: &str = "○";
    /// The switch is on.
    pub const ON: &str = "●";
    /// The switch is off.
    pub const OFF: &str = "◌";
    /// The state is half-way.
    pub const HALF: &str = "◐";
    /// The value is absent.
    pub const NONE: &str = "—";
    /// The value was cut.
    pub const CUT: &str = "…";
}

// ── the width measure ───────────────────────────────────────────────────────

/// The display width of a string: the number of terminal columns it occupies.
///
/// Escape sequences carry no width and are skipped; a styled string measures
/// as its plain text does.
pub fn display_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            skip_escape(&mut chars);
            continue;
        }
        width += char_width(c);
    }
    width
}

/// One character's display width: the number of columns this terminal draws it
/// as.
///
/// East-Asian **Wide**/Fullwidth glyphs and emoji-presentation marks are two,
/// combining marks and control characters are zero, everything else is one.
/// An **ambiguous** mark is one column in a Western terminal and two in a
/// CJK one; the locale decides, and the default is the Western rule.
pub fn char_width(c: char) -> usize {
    let code = c as u32;
    if code < 0x20 || (0x7f..0xa0).contains(&code) {
        return 0;
    }
    if is_zero_width(code) {
        return 0;
    }
    if is_wide(code) {
        return 2;
    }
    1
}

/// The width of the AMBIGUOUS glyphs: two in a CJK locale, one elsewhere.
///
/// The locale is read from the environment; nothing else in the CLI depends
/// on it, so a missing or unusual locale never breaks a render.
pub fn ambiguous_width() -> usize {
    #[cfg(feature = "std")]
    {
        ambiguous_width_for(None)
    }
    // Without the host environment there is no locale to read: the Western
    // rule (one column) is the safe default.
    #[cfg(not(feature = "std"))]
    {
        1
    }
}

/// The ambiguous width for a locale name (`None` reads the environment).
pub fn ambiguous_width_for(locale: Option<&str>) -> usize {
    let locale = match locale {
        Some(locale) => locale.to_string(),
        #[cfg(feature = "std")]
        None => {
            let mut name = String::new();
            for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
                if let Ok(value) = std::env::var(key) {
                    if !value.is_empty() {
                        name = value;
                        break;
                    }
                }
            }
            name
        }
        #[cfg(not(feature = "std"))]
        None => String::new(),
    };
    let cjk = ["zh", "ja", "ko", "zh_CN", "zh_TW", "ja_JP", "ko_KR"]
        .iter()
        .any(|prefix| locale.starts_with(prefix));
    if cjk {
        2
    } else {
        1
    }
}

/// The zero-width ranges: combining marks, variation selectors, zero-width
/// spaces.
fn is_zero_width(code: u32) -> bool {
    matches!(code,
        0x0300..=0x036f
        | 0x0483..=0x0489
        | 0x0591..=0x05bd
        | 0x0610..=0x061a
        | 0x064b..=0x065f
        | 0x0670
        | 0x06d6..=0x06dc
        | 0x0730..=0x074a
        | 0x07a6..=0x07b0
        | 0x0900..=0x0903
        | 0x093a..=0x094f
        | 0x0951..=0x0957
        | 0x1ab0..=0x1aff
        | 0x1dc0..=0x1dff
        | 0x200b..=0x200f
        | 0x2028..=0x202e
        | 0x20d0..=0x20f0
        | 0xfe00..=0xfe0f
        | 0xfe20..=0xfe2f
        | 0xe0100..=0xe01ef
    )
}

/// The wide ranges: East-Asian Wide/Fullwidth and the emoji blocks.
fn is_wide(code: u32) -> bool {
    matches!(code,
        0x1100..=0x115f
        | 0x231a..=0x231b
        | 0x2329..=0x232a
        | 0x23e9..=0x23ec
        | 0x23f0..=0x23f0
        | 0x23f3..=0x23f3
        | 0x25fd..=0x25fe
        | 0x2614..=0x2615
        | 0x2648..=0x2653
        | 0x267f
        | 0x2693
        | 0x26a1
        | 0x26aa..=0x26ab
        | 0x26bd..=0x26be
        | 0x26c4..=0x26c5
        | 0x26ce
        | 0x26d4
        | 0x26ea
        | 0x26f2..=0x26f3
        | 0x26f5
        | 0x26fa
        | 0x26fd
        | 0x2705
        | 0x270a..=0x270b
        | 0x2728
        | 0x274c
        | 0x274e
        | 0x2753..=0x2755
        | 0x2757
        | 0x2795..=0x2797
        | 0x27b0
        | 0x27bf
        | 0x2b1b..=0x2b1c
        | 0x2b50
        | 0x2b55
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xa960..=0xa97f
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe10..=0xfe19
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x16fe0..=0x16fe4
        | 0x17000..=0x187f7
        | 0x18800..=0x18cd5
        | 0x1b000..=0x1b2fb
        | 0x1f004
        | 0x1f0cf
        | 0x1f18e
        | 0x1f191..=0x1f19a
        | 0x1f200..=0x1f320
        | 0x1f32d..=0x1f335
        | 0x1f337..=0x1f37c
        | 0x1f37e..=0x1f393
        | 0x1f3a0..=0x1f3ca
        | 0x1f3cf..=0x1f3d3
        | 0x1f3e0..=0x1f3f0
        | 0x1f3f4
        | 0x1f3f8..=0x1f43e
        | 0x1f440
        | 0x1f442..=0x1f4fc
        | 0x1f4ff..=0x1f53d
        | 0x1f54b..=0x1f54e
        | 0x1f550..=0x1f567
        | 0x1f57a
        | 0x1f595..=0x1f596
        | 0x1f5a4
        | 0x1f5fb..=0x1f64f
        | 0x1f680..=0x1f6c5
        | 0x1f6cc
        | 0x1f6d0..=0x1f6d2
        | 0x1f6d5..=0x1f6d7
        | 0x1f6eb..=0x1f6ec
        | 0x1f6f4..=0x1f6fc
        | 0x1f7e0..=0x1f7eb
        | 0x1f90c..=0x1f93a
        | 0x1f93c..=0x1f945
        | 0x1f947..=0x1f9ff
        | 0x1fa70..=0x1fa74
        | 0x1fa78..=0x1fa7c
        | 0x1fa80..=0x1fa86
        | 0x1fa90..=0x1faac
        | 0x1fab0..=0x1faba
        | 0x1fac0..=0x1fac5
        | 0x1fad0..=0x1fad9
        | 0x1fae0..=0x1fae7
        | 0x1faf0..=0x1faf6
        | 0x20000..=0x2fffd
        | 0x30000..=0x3fffd
    )
}

// ── the width-aware cut ─────────────────────────────────────────────────────

/// Pad a cell to `width` display columns with trailing spaces.
pub fn pad_to(text: &str, width: usize) -> String {
    let used = display_width(text);
    if used >= width {
        return text.to_string();
    }
    let mut out = text.to_string();
    out.push_str(&" ".repeat(width - used));
    out
}

/// Ellipsise a cell to `width` display columns, keeping the escapes intact and
/// marking the cut with `…`.
pub fn truncate_to(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let budget = width.saturating_sub(display_width(glyph::CUT));
    let mut out = String::new();
    let mut used = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            out.push(c);
            copy_escape(&mut chars, &mut out);
            continue;
        }
        let width_of = char_width(c);
        if used + width_of > budget {
            break;
        }
        used += width_of;
        out.push(c);
    }
    out.push_str(glyph::CUT);
    out
}

/// Cut a cell in the MIDDLE: the head and the tail both survive, and the cut
/// is marked. The tail is what an operator scans for, so it keeps its share
/// of the room.
pub fn truncate_middle(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let mark = display_width(glyph::CUT);
    let budget = width.saturating_sub(mark);
    let head_budget = budget.div_ceil(2);
    let tail_budget = budget - head_budget;

    let chars: Vec<char> = text.chars().collect();
    // Walk forward for the head, measuring only the visible characters.
    let mut head = String::new();
    let mut used = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        let c = chars[index];
        if c == '\u{1b}' {
            head.push(c);
            index += 1;
            let mut iter = chars[index..].iter().copied().peekable();
            copy_escape(&mut iter, &mut head);
            index += iter_len(&chars, index);
            continue;
        }
        let width_of = char_width(c);
        if used + width_of > head_budget {
            break;
        }
        used += width_of;
        head.push(c);
        index += 1;
    }
    // Walk backward for the tail.
    let mut tail = String::new();
    let mut tail_used = 0usize;
    let mut back = chars.len();
    while back > index {
        let c = chars[back - 1];
        if c == '\u{1b}' {
            break;
        }
        let width_of = char_width(c);
        if tail_used + width_of > tail_budget {
            break;
        }
        tail_used += width_of;
        tail.insert(0, c);
        back -= 1;
    }
    let mut out = head;
    out.push_str(glyph::CUT);
    out.push_str(&tail);
    out
}

/// Cut a path so its BASENAME survives: the head keeps its leading
/// directories, the tail keeps the name.
pub fn truncate_path(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let mark = display_width(glyph::CUT);
    let budget = width.saturating_sub(mark);
    let (dir, base) = match text.rsplit_once('/') {
        Some((dir, base)) => (dir, base),
        None => return truncate_middle(text, width),
    };
    let base_width = display_width(base);
    if base_width + 1 > budget {
        return truncate_middle(text, width);
    }
    let head_budget = budget - base_width - 1;
    let mut head = String::new();
    let mut used = 0usize;
    for c in dir.chars() {
        let width_of = char_width(c);
        if used + width_of > head_budget {
            break;
        }
        used += width_of;
        head.push(c);
    }
    let mut out = head;
    if !out.is_empty() {
        out.push('/');
    }
    out.push_str(glyph::CUT);
    out.push_str(base);
    out
}

/// How many characters an escape consumed (the iterator consumed them).
fn iter_len(chars: &[char], start: usize) -> usize {
    // The escape was copied through the iterator; count what it ate.
    let mut count = 0usize;
    let mut iter = chars[start..].iter().copied().peekable();
    let mut sink = String::new();
    copy_escape(&mut iter, &mut sink);
    count += iter.count();
    count
}

/// Consume an ANSI escape sequence whose leading ESC was already taken.
fn skip_escape<I>(chars: &mut core::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
        Some(']') => {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '\u{07}' {
                    break;
                }
                if c == '\u{1b}' {
                    if let Some('\\') = chars.peek() {
                        chars.next();
                    }
                    break;
                }
            }
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

/// The same consumption, copying the sequence through so a style survives.
fn copy_escape<I>(chars: &mut core::iter::Peekable<I>, out: &mut String)
where
    I: Iterator<Item = char>,
{
    match chars.peek().copied() {
        Some('[') => {
            out.push(chars.next().unwrap());
            for c in chars.by_ref() {
                out.push(c);
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
        Some(']') => {
            out.push(chars.next().unwrap());
            while let Some(c) = chars.next() {
                out.push(c);
                if c == '\u{07}' {
                    break;
                }
                if c == '\u{1b}' {
                    if let Some('\\') = chars.peek() {
                        out.push(chars.next().unwrap());
                    }
                    break;
                }
            }
        }
        Some(_) => {
            out.push(chars.next().unwrap());
        }
        None => {}
    }
}

// ── the box table ───────────────────────────────────────────────────────────

/// The most one column draws before its cells are cut.
pub const DEFAULT_CELL_CAP: usize = 48;

/// A table whose widths are measured, not guessed.
pub struct Table {
    /// The column headers.
    pub headers: Vec<String>,
    rows: Vec<Vec<Cell>>,
    caps: Vec<usize>,
}

impl Table {
    /// A table with the given headers.
    pub fn new(headers: Vec<String>) -> Self {
        Table {
            caps: vec![DEFAULT_CELL_CAP; headers.len()],
            headers,
            rows: Vec::new(),
        }
    }

    /// The most one column draws: its cells are cut to this width, the cut
    /// marked, and the hidden rest lives in the JSON form.
    pub fn cap(mut self, column: usize, max: usize) -> Self {
        if column < self.caps.len() {
            self.caps[column] = max;
        }
        self
    }

    /// Add a row.
    pub fn row<I>(mut self, cells: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Cell>,
    {
        self.rows.push(cells.into_iter().map(Into::into).collect());
        self
    }

    /// The measured width of every column.
    pub fn widths(&self) -> Vec<usize> {
        let mut widths: Vec<usize> = self.headers.iter().map(|h| display_width(h)).collect();
        for row in &self.rows {
            for (index, cell) in row.iter().enumerate() {
                let width = display_width(cell.content());
                if index < widths.len() && width > widths[index] {
                    widths[index] = width;
                }
            }
        }
        for (index, width) in widths.iter_mut().enumerate() {
            let cap = self.caps.get(index).copied().unwrap_or(DEFAULT_CELL_CAP);
            *width = (*width).min(cap);
        }
        widths
    }

    /// Draw the table.
    pub fn render(&self, paint: &Paint) -> String {
        let widths = self.widths();
        let mut out = String::new();
        out.push_str("  ┌");
        out.push_str(&join_segments(&widths, "┬", "─"));
        out.push_str("┐\n");
        out.push_str("  │ ");
        let headers: Vec<String> = self
            .headers
            .iter()
            .enumerate()
            .map(|(index, header)| {
                let width = widths.get(index).copied().unwrap_or(0);
                pad_to(&truncate_to(header, width), width)
            })
            .collect();
        out.push_str(&headers.join(" │ "));
        out.push_str(" │\n");
        out.push_str("  ├");
        out.push_str(&join_segments(&widths, "┼", "─"));
        out.push_str("┤\n");
        for row in &self.rows {
            out.push_str("  │ ");
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(index, cell)| {
                    let width = widths.get(index).copied().unwrap_or(0);
                    let content = pad_to(&cell.cut(width), width);
                    match cell.style() {
                        Sgr::Plain => content,
                        style => paint.paint(&content, style_code(style)),
                    }
                })
                .collect();
            out.push_str(&cells.join(" │ "));
            out.push_str(" │\n");
        }
        out.push_str("  └");
        out.push_str(&join_segments(&widths, "┴", "─"));
        out.push('┘');
        out
    }
}

/// The ANSI code of a style (the CLI's own paint table).
fn style_code(style: Sgr) -> u8 {
    match style {
        Sgr::Plain => 0,
        Sgr::Green => 32,
        Sgr::Yellow => 33,
        Sgr::Red => 31,
        Sgr::Cyan => 36,
        Sgr::Bold => 1,
        Sgr::Dimmed => 2,
    }
}

/// The box's horizontal segments.
fn join_segments(widths: &[usize], separator: &str, fill: &str) -> String {
    widths
        .iter()
        .map(|width| fill.repeat(width + 2))
        .collect::<Vec<_>>()
        .join(separator)
}

/// A byte count, in the units an operator reads.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_width_is_measured_in_columns() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("◉ active"), 8);
        assert_eq!(display_width("日本"), 4);
        assert_eq!(display_width("e\u{301}"), 1);
    }

    #[test]
    fn a_style_is_not_part_of_a_width() {
        let styled = "\u{1b}[32m◉ active\u{1b}[0m";
        assert_eq!(display_width(styled), display_width("◉ active"));
    }

    #[test]
    fn the_middle_cut_keeps_both_ends() {
        let cut = truncate_middle("pool_workqueue_release_worker", 20);
        assert!(cut.contains('…'), "{cut}");
        assert!(cut.starts_with("pool_work"), "{cut}");
        assert!(cut.ends_with("worker"), "{cut}");
        assert!(display_width(&cut) <= 20, "{cut}");
    }

    #[test]
    fn a_path_keeps_its_basename() {
        let cut = truncate_path("/very/long/directory/path/to/a/file.txt", 20);
        assert!(cut.ends_with("file.txt"), "{cut}");
        assert!(display_width(&cut) <= 20, "{cut}");
    }

    #[test]
    fn the_table_measures_its_columns() {
        let table = Table::new(vec!["NAME".to_string(), "STATE".to_string()])
            .row(vec![Cell::plain("wlan0"), Cell::styled("◉ up", Sgr::Green)])
            .row(vec![Cell::plain("eth0"), Cell::plain("○ down")]);
        let drawn = table.render(&Paint::plain());
        assert!(drawn.contains("┌"), "{drawn}");
        assert!(drawn.contains("wlan0"), "{drawn}");
        assert!(drawn.contains("◉ up"), "{drawn}");
    }

    #[test]
    fn the_glyphs_are_one_per_state() {
        assert_eq!(glyph::ACTIVE, "◉");
        assert_eq!(glyph::UNKNOWN, "○");
        assert_eq!(glyph::ON, "●");
        assert_eq!(glyph::OFF, "◌");
        assert_eq!(glyph::HALF, "◐");
    }
}
