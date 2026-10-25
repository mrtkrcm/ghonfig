//! The interactive configuration browser.
//!
//! Two panes: the settings on the left, Ghostty's own documentation for the
//! selected one on the right. That is the whole idea — the docs for these 200
//! keys otherwise exist only inside the binary, so browsing them beside your
//! actual values is most of the value the tool has to offer.
//!
//! Every picker in here is filled by asking Ghostty: themes and their palettes,
//! font families and their styles, colour names, keybind actions, and the valid
//! values of all 48 enum keys. Nothing is enumerated by hand.

use crate::check::{self, Check};
use crate::config::{ConfigFile, Entry};
use crate::ghostty::{FontFamily, FontStatus, Ghostty, Kind, Schema};
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Alignment, Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Clear, List, ListItem, ListState, Padding, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Tabs, Wrap,
    },
    DefaultTerminal, Frame,
};
use ratatui_textarea::TextArea;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};
use throbber_widgets_tui::{Throbber, ThrobberState, BRAILLE_SIX};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
const TOAST_TTL: Duration = Duration::from_secs(5);

// All leads: that is the landing tab, with commons pinned above the schema.
const TABS: [&str; 3] = ["All", "Set", "Check"];

/// Keys hoisted above the schema order in the All tab.
///
/// This is a list of key *names*, not of values — the thing the rest of this
/// program refuses to hardcode is values, because those drift against what is
/// installed. A name that Ghostty drops simply stops matching the schema and
/// disappears from the pinned block, so this cannot go stale in the way a
/// hand-written font list can. It is an opinion about what people reach for
/// first, and Ghostty has no opinion to ask it for.
const PINNED: [&str; 12] = [
    "theme",
    "font-family",
    "font-size",
    "background-opacity",
    "background-blur",
    "cursor-style",
    "window-padding-x",
    "window-padding-y",
    "window-decoration",
    "macos-titlebar-style",
    "keybind",
    "shell-integration-features",
];

enum Loaded {
    Schema(Box<Schema>),
    Themes(Vec<String>),
    Fonts(Vec<FontFamily>),
    Colors(Vec<(String, String)>),
    Actions(Vec<String>),
}

/// What the picker shows in its right-hand pane. Themes and colours are the two
/// cases where the value *is* something you can look at, so it is shown.
/// Where a picker's values come from. Borrowed, never copied: `pickable` runs
/// for every visible row of every frame.
enum Source<'a> {
    Themes,
    Fonts,
    Colors,
    Values(&'a [String]),
    /// Ghostty describes this key only in prose — Enter opens a text field.
    Free,
}

#[derive(PartialEq, Clone, Copy)]
enum Preview {
    None,
    Theme,
    Font,
    Color,
}

struct Picker {
    key: String,
    items: Vec<String>,
    /// The value the config already holds, marked in the list. Once a filter
    /// has moved the cursor off it, this is the only way to see where you were.
    current: String,
    filter: String,
    state: ListState,
    preview: Preview,
    /// Whether a value outside `items` is legal — true for fonts and colours
    /// (any string, any hex), false for a closed enum.
    free: bool,
}

impl Picker {
    fn visible(&self) -> Vec<&String> {
        let mut hits: Vec<(i32, &String)> = self
            .items
            .iter()
            .filter_map(|i| fuzzy(i, &self.filter).map(|s| (s, i)))
            .collect();
        if !self.filter.is_empty() {
            hits.sort_by(|a, b| b.0.cmp(&a.0));
        }
        hits.into_iter().map(|(_, i)| i).collect()
    }
}

enum Mode {
    Browse,
    Search,
    Help,
    /// A real editor rather than an append-only buffer: half these values are
    /// long comma lists (`shell-integration-features`) where retyping from
    /// scratch to change one word is the whole friction.
    Edit { key: String, area: Box<TextArea<'static>> },
    Pick(Box<Picker>),
}

enum Row {
    /// A non-selectable divider. Only two exist, around the pinned block.
    Header(String),
    Setting {
        key: String,
        value: String,
        /// Index into the config file, when the key is actually assigned there.
        idx: Option<usize>,
        /// Whether Enter opens a list rather than a text field.
        pickable: bool,
    },
}

impl Row {
    fn key(&self) -> &str {
        match self {
            Row::Header(_) => "",
            Row::Setting { key, .. } => key,
        }
    }
}

pub struct App {
    gh: Option<Ghostty>,
    cfg: ConfigFile,
    schema: Option<Schema>,
    themes: Vec<String>,
    fonts: Vec<FontFamily>,
    colors: Vec<(String, String)>,
    actions: Vec<String>,
    pending: usize,
    rx: Receiver<Loaded>,
    tab: usize,
    list: ListState,
    filter: String,
    mode: Mode,
    toast: Option<(String, bool, Instant)>,
    checks: Option<Vec<Check>>,
    check_rx: Option<Receiver<Vec<Check>>>,
    /// Ghostty's documentation runs to hundreds of lines for keys like
    /// `keybind`, so the docs pane scrolls independently of the list.
    doc_scroll: u16,
    doc_lines: usize,
    throbber: ThrobberState,
    quit: bool,
}

impl App {
    pub fn new(gh: Option<Ghostty>, cfg: ConfigFile) -> Self {
        // `+list-fonts` costs ~9.6s cold and `+list-themes` ~2.4s, so nothing
        // waits on them: the UI opens immediately and the pickers fill in.
        let (tx, rx) = channel();
        let mut pending = 0;
        if let Some(g) = gh.clone() {
            pending = 5;
            std::thread::spawn(move || {
                // Schema first: it carries the docs and the value kinds, which
                // is what the first frame after startup actually needs.
                if let Ok(s) = g.schema() {
                    let _ = tx.send(Loaded::Schema(Box::new(Schema::new(s))));
                }
                if let Ok(t) = g.themes() {
                    let _ = tx.send(Loaded::Themes(t));
                }
                if let Ok(c) = g.colors() {
                    let _ = tx.send(Loaded::Colors(c));
                }
                if let Ok(a) = g.actions() {
                    let _ = tx.send(Loaded::Actions(a));
                }
                if let Ok(f) = g.fonts() {
                    let _ = tx.send(Loaded::Fonts(f));
                }
            });
        }
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            gh,
            cfg,
            schema: None,
            themes: vec![],
            fonts: vec![],
            colors: vec![],
            actions: vec![],
            pending,
            rx,
            tab: 0,
            list,
            filter: String::new(),
            mode: Mode::Browse,
            toast: None,
            checks: None,
            check_rx: None,
            doc_scroll: 0,
            doc_lines: 0,
            throbber: ThrobberState::default(),
            quit: false,
        }
    }

    pub fn run(mut self, term: &mut DefaultTerminal) -> anyhow::Result<()> {
        while !self.quit {
            term.draw(|f| self.draw(f))?;
            self.drain();
            if event::poll(Duration::from_millis(120))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        self.key(k);
                    }
                }
            }
        }
        Ok(())
    }

    /// Collect whatever the background threads have finished.
    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match msg {
                Loaded::Schema(s) => {
                    self.schema = Some(*s);
                    // Rows for All only exist once the schema arrives; skip the
                    // ★ common header so the cursor lands on `theme`.
                    if self.tab < 2 {
                        self.select_first_setting();
                    }
                }
                Loaded::Themes(t) => self.themes = t,
                Loaded::Fonts(f) => self.fonts = f,
                Loaded::Colors(c) => self.colors = c,
                Loaded::Actions(a) => self.actions = a,
            }
        }
        if let Some(rx) = &self.check_rx {
            if let Ok(c) = rx.try_recv() {
                self.checks = Some(c);
                self.check_rx = None;
            }
        }
        // A toast that never leaves reads as part of the chrome, and the next
        // one then looks like it did not fire.
        if self.toast.as_ref().is_some_and(|(_, _, at)| at.elapsed() > TOAST_TTL) {
            self.toast = None;
        }
    }

    fn repeatable(&self, key: &str) -> bool {
        self.schema.as_ref().is_some_and(|s| s.is_repeatable(key))
    }

    fn kind(&self, key: &str) -> Kind {
        self.schema.as_ref().map_or(Kind::Text, |s| s.kind(key))
    }

    /// Where a key's offered values come from. One place decides this, so the
    /// `▸` marker and the picker can never disagree about whether Enter opens
    /// a list — and asking costs a borrow rather than a copy of 537 strings.
    fn source(&self, key: &str) -> Source<'_> {
        match key {
            "theme" => Source::Themes,
            k if k.contains("font-family") => Source::Fonts,
            k => match self.schema.as_ref().and_then(|s| s.get(k)).map(|s| &s.kind) {
                Some(Kind::Enum(v)) => Source::Values(v),
                Some(Kind::Color) => Source::Colors,
                _ => Source::Free,
            },
        }
    }

    /// Asked once per visible row per frame, so it must not build the list to
    /// find out whether the list would be empty.
    fn pickable(&self, key: &str) -> bool {
        match self.source(key) {
            Source::Themes => !self.themes.is_empty(),
            Source::Fonts => !self.fonts.is_empty(),
            Source::Colors => !self.colors.is_empty(),
            Source::Values(v) => !v.is_empty(),
            Source::Free => false,
        }
    }

    /// The values themselves, built only when the picker actually opens.
    fn options(&self, key: &str) -> (Vec<String>, Preview, bool) {
        match self.source(key) {
            Source::Themes => (self.themes.clone(), Preview::Theme, false),
            Source::Fonts => (
                self.fonts.iter().map(|f| f.name.clone()).collect(),
                Preview::Font,
                true,
            ),
            Source::Colors => (
                self.colors.iter().map(|(n, _)| n.clone()).collect(),
                Preview::Color,
                true,
            ),
            Source::Values(v) => (v.to_vec(), Preview::None, false),
            Source::Free => (vec![], Preview::None, true),
        }
    }

    fn hex_of(&self, name: &str) -> Option<String> {
        if name.starts_with('#') {
            return Some(name.to_string());
        }
        self.colors
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, h)| h.clone())
    }

    // ── rows ────────────────────────────────────────────────────────────────

    fn rows(&self) -> Vec<Row> {
        let setting = |key: &str, idx: Option<usize>, value: String| Row::Setting {
            pickable: self.pickable(key),
            key: key.to_string(),
            value,
            idx,
        };
        match self.tab {
            // Every key Ghostty has, with your value if you set one. Pinned keys
            // sit once at the top; while filtering the block is dropped entirely
            // so a search hit is not echoed above the results.
            0 => {
                let Some(schema) = self.schema.as_ref() else { return vec![] };
                let row_for = |key: &str| {
                    let idx = self.cfg.effective(key, self.repeatable(key));
                    let value = idx
                        .map(|i| self.cfg.value_at(i).to_string())
                        .unwrap_or_else(|| {
                            schema.get(key).map(|s| s.default_display()).unwrap_or_default()
                        });
                    self.keep(key, &value).then(|| setting(key, idx, value))
                };
                let mut out = Vec::new();
                let pin = self.filter.is_empty();
                if pin {
                    let pinned: Vec<Row> = PINNED
                        .iter()
                        .filter(|k| schema.get(k).is_some())
                        .filter_map(|k| row_for(k))
                        .collect();
                    if !pinned.is_empty() {
                        out.push(Row::Header("common".into()));
                        out.extend(pinned);
                        out.push(Row::Header("all".into()));
                    }
                }
                out.extend(schema.settings.iter().filter_map(|s| {
                    if pin && PINNED.contains(&s.key.as_str()) {
                        return None;
                    }
                    row_for(&s.key)
                }));
                out
            }
            // Only what this config actually assigns — one row per line, so a
            // key assigned twice is visibly assigned twice.
            1 => self
                .cfg
                .entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| match e {
                    Entry::Setting { key, value } if self.keep(key, value) => {
                        Some(setting(key, Some(i), value.clone()))
                    }
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }

    fn keep(&self, key: &str, value: &str) -> bool {
        self.filter.is_empty()
            || fuzzy(key, &self.filter).is_some()
            || value.to_lowercase().contains(&self.filter.to_lowercase())
    }

    fn selected_row(&self) -> Option<Row> {
        let rows = self.rows();
        self.list.selected().and_then(|i| rows.into_iter().nth(i))
    }

    // ── actions ─────────────────────────────────────────────────────────────

    fn open_editor(&mut self) {
        let Some(row) = self.selected_row() else { return };
        let Row::Setting { key, idx, .. } = row else { return };
        let current = idx.map(|i| self.cfg.value_at(i).to_string()).unwrap_or_default();

        let (items, preview, free) = self.options(&key);
        if items.is_empty() {
            self.mode = Mode::Edit { key, area: Box::new(new_textarea(&current)) };
            return;
        }
        let mut state = ListState::default();
        state.select(Some(items.iter().position(|i| *i == current).unwrap_or(0)));
        self.mode = Mode::Pick(Box::new(Picker {
            key,
            items,
            current,
            filter: String::new(),
            state,
            preview,
            free,
        }));
    }

    fn apply(&mut self, key: &str, value: &str) {
        let repeatable = self.repeatable(key);
        // Edit the exact line the cursor is on when there is one, so editing the
        // second of two assignments does not silently rewrite the first.
        match self
            .selected_row()
            .and_then(|r| (r.key() == key).then_some(r))
            .and_then(|r| match r {
                Row::Setting { idx, .. } => idx,
                Row::Header(_) => None,
            }) {
            Some(i) => self.cfg.set_at(i, value),
            None => self.cfg.set(key, value, repeatable),
        }

        // A font Ghostty cannot render is accepted silently and falls back, so
        // this is the one moment the mistake is visible.
        if key.contains("font-family") && !value.is_empty() {
            if let Some(gh) = &self.gh {
                match gh.font_renderable(value) {
                    FontStatus::Missing => {
                        self.toast(format!("'{value}' will NOT render — Ghostty falls back"), true);
                        return;
                    }
                    FontStatus::Unknown => {
                        self.toast(format!("{key} = {value} (could not verify it renders)"), false);
                        return;
                    }
                    FontStatus::Renderable => {}
                }
            }
        }
        self.toast(format!("{key} = {value}"), false);
    }

    fn unset(&mut self) {
        let Some(Row::Setting { key, idx, .. }) = self.selected_row() else { return };
        match idx {
            Some(i) => {
                self.cfg.remove_at(i);
                self.toast(format!("unset {key} — back to Ghostty's default"), false);
            }
            None => self.toast(format!("{key} is not set here"), false),
        }
    }

    fn save(&mut self) -> bool {
        match self.cfg.save(self.gh.as_ref()) {
            Ok(()) => {
                self.toast("saved — reload Ghostty with Cmd+Shift+,".into(), false);
                true
            }
            Err(e) => {
                self.toast(e.to_string().lines().next().unwrap_or("invalid").to_string(), true);
                false
            }
        }
    }

    /// Write dirty edits, then leave. An invalid config stays open so the
    /// mistake is still on screen rather than discarded by the quit.
    fn quit_saving(&mut self) {
        if self.cfg.dirty && !self.save() {
            return;
        }
        self.quit = true;
    }

    fn run_checks(&mut self) {
        if self.check_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let gh = self.gh.clone();
        // The checks re-read the file from disk deliberately: they report on
        // what Ghostty would load, not on unsaved edits.
        let path = self.cfg.path.clone();
        let schema = gh.as_ref().and_then(|g| g.schema().ok()).map(Schema::new);
        std::thread::spawn(move || {
            let cfg = ConfigFile::load(path).unwrap_or_else(|_| ConfigFile {
                path: Default::default(),
                entries: vec![],
                dirty: false,
            });
            let _ = tx.send(check::run(gh.as_ref(), schema.as_ref(), &cfg));
        });
        self.check_rx = Some(rx);
        self.checks = None;
    }

    fn toast(&mut self, msg: String, err: bool) {
        self.toast = Some((msg, err, Instant::now()));
    }

    /// Move the cursor, stepping over the two group headers rather than letting
    /// them be selected — a header has no docs and nothing to edit.
    fn move_sel(&mut self, delta: isize, rows: &[Row]) {
        self.doc_scroll = 0;
        if rows.is_empty() {
            self.list.select(Some(0));
            return;
        }
        let last = rows.len() as isize - 1;
        let mut i = (self.list.selected().unwrap_or(0) as isize + delta).clamp(0, last);
        let step = if delta >= 0 { 1 } else { -1 };
        // Bounded because this runs inside the draw path: a list of nothing but
        // headers would otherwise bounce between two of them forever.
        let mut hops = 0;
        while matches!(rows.get(i as usize), Some(Row::Header(_))) && hops <= last {
            hops += 1;
            let next = i + step;
            // At an edge, a header can only be escaped by turning around.
            i = if (0..=last).contains(&next) { next } else { i - step };
        }
        self.list.select(Some(i.clamp(0, last) as usize));
    }

    // ── input ───────────────────────────────────────────────────────────────

    fn key(&mut self, k: KeyEvent) {
        match &mut self.mode {
            Mode::Browse => self.key_browse(k),
            Mode::Help => self.mode = Mode::Browse,
            Mode::Search => match k.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.mode = Mode::Browse;
                }
                KeyCode::Enter | KeyCode::Down | KeyCode::Up => self.mode = Mode::Browse,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.select_first_setting();
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter.clear();
                    self.select_first_setting();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.select_first_setting();
                }
                _ => {}
            },
            Mode::Edit { key, area } => match k.code {
                KeyCode::Esc => self.mode = Mode::Browse,
                // The editor opens with the cursor at the end of the existing
                // value, so typing appends. ratatui-textarea has no ^U of its
                // own and that is the reflex for "clear this field".
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    **area = new_textarea("");
                }
                // Intercepted before the textarea sees it: a config value is one
                // line, so Enter means "apply", never "insert a newline".
                KeyCode::Enter => {
                    let (key, value) = (key.clone(), area.lines().join(" "));
                    self.mode = Mode::Browse;
                    self.apply(&key, value.trim());
                }
                _ => {
                    area.input(k);
                }
            },
            Mode::Pick(p) => {
                let len = p.visible().len();
                match k.code {
                    KeyCode::Esc => self.mode = Mode::Browse,
                    KeyCode::Up => p.state.select(Some(p.state.selected().unwrap_or(0).saturating_sub(1))),
                    KeyCode::Down => {
                        let i = p.state.selected().unwrap_or(0) + 1;
                        p.state.select(Some(i.min(len.saturating_sub(1))));
                    }
                    KeyCode::PageUp => p.state.select(Some(p.state.selected().unwrap_or(0).saturating_sub(10))),
                    KeyCode::PageDown => {
                        let i = p.state.selected().unwrap_or(0) + 10;
                        p.state.select(Some(i.min(len.saturating_sub(1))));
                    }
                    KeyCode::Home => p.state.select(Some(0)),
                    KeyCode::End => p.state.select(Some(len.saturating_sub(1))),
                    KeyCode::Enter => {
                        // For an open-ended key the filter itself is a legal
                        // value — that is how `#1a2b3c` or a font this build of
                        // Ghostty has not indexed gets in.
                        let pick = p
                            .state
                            .selected()
                            .and_then(|i| p.visible().get(i).map(|s| (*s).clone()))
                            .or_else(|| (p.free && !p.filter.is_empty()).then(|| p.filter.clone()));
                        let key = p.key.clone();
                        self.mode = Mode::Browse;
                        if let Some(v) = pick {
                            self.apply(&key, &v);
                        }
                    }
                    KeyCode::Backspace => {
                        p.filter.pop();
                        p.state.select(Some(0));
                    }
                    KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        p.filter.clear();
                        p.state.select(Some(0));
                    }
                    KeyCode::Char(c) => {
                        p.filter.push(c);
                        p.state.select(Some(0));
                    }
                    _ => {}
                }
            }
        }
    }

    fn select_first_setting(&mut self) {
        let rows = self.rows();
        self.list.select(Some(0));
        self.move_sel(0, &rows);
    }

    fn key_browse(&mut self, k: KeyEvent) {
        let rows = if self.tab == 2 { vec![] } else { self.rows() };
        let len = if self.tab == 2 {
            self.checks.as_ref().map_or(0, |c| c.len())
        } else {
            rows.len()
        };
        let step = |app: &mut Self, d: isize| {
            if app.tab == 2 {
                let cur = app.list.selected().unwrap_or(0) as isize;
                app.list.select(Some(cur.saturating_add(d).clamp(0, len as isize - 1).max(0) as usize));
            } else {
                app.move_sel(d, &rows);
            }
        };
        match k.code {
            KeyCode::Char('q') => self.quit_saving(),
            // Uppercase Q and ^C discard — the escape hatches when a write
            // would be wrong and you want out without touching the file.
            KeyCode::Char('Q') => self.quit = true,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.go_tab(1),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => self.go_tab(-1),
            KeyCode::Down | KeyCode::Char('j') => step(self, 1),
            KeyCode::Up | KeyCode::Char('k') => step(self, -1),
            KeyCode::Char('d') if k.modifiers.contains(KeyModifiers::CONTROL) => step(self, 10),
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => step(self, -10),
            // The docs pane scrolls on its own keys: `keybind` alone documents
            // 93 defaults, far more than fits beside the list.
            KeyCode::PageDown => self.doc_scroll = (self.doc_scroll + 8).min(self.doc_lines as u16),
            KeyCode::PageUp => self.doc_scroll = self.doc_scroll.saturating_sub(8),
            KeyCode::Home | KeyCode::Char('g') => {
                self.list.select(Some(0));
                self.move_sel(0, &rows);
                self.doc_scroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') => step(self, len as isize),
            KeyCode::Char('/') => {
                self.filter.clear();
                self.mode = Mode::Search;
            }
            KeyCode::Esc => {
                self.filter.clear();
                self.toast = None;
            }
            KeyCode::Enter if self.tab < 2 => self.open_editor(),
            KeyCode::Char('d') if self.tab < 2 => self.unset(),
            KeyCode::Char('s') => {
                let _ = self.save();
            }
            KeyCode::Char('r') if self.tab == 2 => self.run_checks(),
            KeyCode::Char('R') => {
                if let Some(gh) = &self.gh {
                    gh.clear_cache();
                }
                self.toast("caches cleared — restart ghonfig to re-read fonts/themes".into(), false);
            }
            _ => {}
        }
    }

    fn go_tab(&mut self, d: isize) {
        self.tab = (self.tab as isize + d).rem_euclid(TABS.len() as isize) as usize;
        self.doc_scroll = 0;
        self.select_first_setting();
        if self.tab == 2 {
            self.run_checks();
        }
    }

    // ── drawing ─────────────────────────────────────────────────────────────

    fn draw(&mut self, f: &mut Frame) {
        self.throbber.calc_next();
        let [header, body, toast, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(f.area());

        self.draw_header(f, header);
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(46), Constraint::Min(0)]).areas(body);

        if self.tab == 2 {
            self.draw_checks(f, left, right);
        } else {
            self.draw_settings(f, left, right);
        }

        if let Some((msg, err, _)) = &self.toast {
            let style = if *err { Style::new().red() } else { Style::new().fg(ACCENT) };
            f.render_widget(Paragraph::new(Line::from(format!(" {msg}")).style(style)), toast);
        }
        f.render_widget(Paragraph::new(self.footer()), footer);

        if matches!(self.mode, Mode::Help) {
            self.draw_help(f);
        }
    }

    fn draw_header(&mut self, f: &mut Frame, area: Rect) {
        let [title_row, tab_row, _] =
            Layout::vertical([Constraint::Length(1); 3]).areas(area);

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled("ghonfig", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled("  ? for keys", Style::new().fg(MUTED)),
            ])),
            title_row,
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(pretty_path(&self.cfg.path), Style::new().fg(MUTED)),
                Span::styled(
                    if self.cfg.dirty { " ● unsaved" } else { "" },
                    Style::new().fg(Color::Yellow),
                ),
                Span::raw(" "),
            ]))
            .alignment(Alignment::Right),
            title_row,
        );

        let [tabs_area, spinner] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(14)]).areas(tab_row);

        let titles: Vec<String> = TABS
            .iter()
            .enumerate()
            .map(|(i, t)| match i {
                0 => match &self.schema {
                    Some(s) => format!("{t} {}", s.settings.len()),
                    None => t.to_string(),
                },
                1 => format!("{t} {}", self.cfg.positions_count()),
                _ => t.to_string(),
            })
            .collect();
        f.render_widget(
            Tabs::new(titles)
                .select(self.tab)
                .style(Style::new().fg(MUTED))
                .highlight_style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
                .divider(Span::styled("·", Style::new().fg(MUTED)))
                .padding(" ", " "),
            tabs_area,
        );

        // The font list costs ~9.6s on a cold cache. A spinner is the difference
        // between "still working" and "the pickers are broken".
        if self.pending > 0 {
            f.render_stateful_widget(
                Throbber::default()
                    .label("loading")
                    .style(Style::new().fg(MUTED))
                    .throbber_style(Style::new().fg(ACCENT))
                    .throbber_set(BRAILLE_SIX),
                spinner,
                &mut self.throbber,
            );
        }
    }

    fn draw_settings(&mut self, f: &mut Frame, left: Rect, right: Rect) {
        let rows = self.rows();
        let width = left.width.saturating_sub(4) as usize;
        let keyw = width.clamp(12, 28);

        let items: Vec<ListItem> = rows
            .iter()
            .map(|r| match r {
                Row::Header(t) => ListItem::new(match t.as_str() {
                    "common" => Line::from(vec![
                        Span::styled("★ ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                        Span::styled(
                            "common",
                            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    _ => Line::from(vec![
                        Span::styled("── ", Style::new().fg(MUTED)),
                        Span::styled("all settings", Style::new().fg(MUTED)),
                    ]),
                }),
                Row::Setting { key, value, idx, pickable } => {
                    let value = truncate(value, width.saturating_sub(keyw + 3));
                    let vstyle = if idx.is_some() {
                        Style::new().fg(Color::White)
                    } else {
                        // Showing Ghostty's default, not your value.
                        Style::new().fg(MUTED).add_modifier(Modifier::ITALIC)
                    };
                    ListItem::new(Line::from(vec![
                        // A dot for "you set this", so the All tab shows at a
                        // glance which of the 200 keys you actually own.
                        Span::styled(
                            if idx.is_some() { "● " } else { "  " },
                            Style::new().fg(Color::Yellow),
                        ),
                        Span::styled(
                            format!("{:<keyw$} ", truncate(key, keyw)),
                            Style::new().fg(ACCENT),
                        ),
                        Span::styled(value, vstyle),
                        Span::styled(
                            if *pickable { " ▸" } else { "" },
                            Style::new().fg(MUTED),
                        ),
                    ]))
                }
            })
            .collect();

        let count = rows.iter().filter(|r| matches!(r, Row::Setting { .. })).count();
        let pinned_n = if self.tab == 0 && self.filter.is_empty() {
            PINNED.iter().filter(|k| self.schema.as_ref().is_some_and(|s| s.get(k).is_some())).count()
        } else {
            0
        };
        let title = if matches!(self.mode, Mode::Search) || !self.filter.is_empty() {
            format!(" /{}▏ {count} ", self.filter)
        } else if pinned_n > 0 {
            format!(" {pinned_n} pinned · {count} ")
        } else {
            format!(" {count} ")
        };
        f.render_stateful_widget(
            List::new(items)
                .block(pane(&title))
                .highlight_style(Style::new().fg(Color::Black).bg(ACCENT))
                .highlight_symbol(""),
            left,
            &mut self.list,
        );
        scrollbar(f, left, rows.len(), self.list.selected().unwrap_or(0));

        // An empty pane reads as a broken one. Say which of the three reasons
        // it is, since each has a different next keystroke.
        if rows.is_empty() {
            let hint = match (self.filter.is_empty(), self.tab, self.schema.is_some()) {
                (false, ..) => "no key matches — esc to clear the search",
                (_, 1, _) => "nothing set here — ← for all of Ghostty's settings",
                (_, _, false) => "waiting for Ghostty…",
                _ => "",
            };
            f.render_widget(
                Paragraph::new(Line::styled(hint, Style::new().fg(MUTED)))
                    .wrap(Wrap { trim: false }),
                left.inner(Margin { vertical: 1, horizontal: 2 }),
            );
        }

        // Right pane: Ghostty's documentation for the selected key.
        let (title, text) = match rows.get(self.list.selected().unwrap_or(0)) {
            Some(Row::Setting { key, .. }) => (format!(" {key} "), self.doc_for(key)),
            _ => (" ".into(), Text::raw("")),
        };
        self.doc_lines = text.lines.len();
        f.render_widget(
            Paragraph::new(text)
                .block(pane(&title))
                .wrap(Wrap { trim: false })
                .scroll((self.doc_scroll, 0)),
            right,
        );
        scrollbar(f, right, self.doc_lines, self.doc_scroll as usize);

        match &self.mode {
            Mode::Edit { key, area } => self.draw_edit(f, key, area),
            Mode::Pick(p) => self.draw_pick(f, p),
            _ => {}
        }
    }

    /// Default, accepted values, then Ghostty's prose. The accepted-values line
    /// is the part that used to require guessing or a trip to the website.
    fn doc_for(&self, key: &str) -> Text<'static> {
        let Some(setting) = self.schema.as_ref().and_then(|s| s.get(key)) else {
            return Text::from(if self.pending > 0 {
                Line::styled("loading Ghostty's docs…", Style::new().fg(MUTED))
            } else {
                Line::styled("No documentation — Ghostty does not know this key.", Style::new().red())
            });
        };
        let label = |t: &str| Span::styled(format!("{t:<9}"), Style::new().fg(MUTED));
        let mut lines = vec![];

        let d = setting.default_display();
        if !d.is_empty() {
            lines.push(Line::from(vec![label("default"), Span::raw(d)]));
        }
        match &setting.kind {
            Kind::Enum(values) => {
                let mut spans = vec![label("accepts")];
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::styled(" · ", Style::new().fg(MUTED)));
                    }
                    spans.push(Span::styled(v.clone(), Style::new().fg(Color::Green)));
                }
                lines.push(Line::from(spans));
            }
            Kind::Color => lines.push(Line::from(vec![
                label("accepts"),
                Span::styled("#rrggbb", Style::new().fg(Color::Green)),
                Span::styled(
                    format!(" or one of {} colour names", self.colors.len().max(1)),
                    Style::new().fg(MUTED),
                ),
            ])),
            Kind::Structured => lines.push(Line::from(vec![
                label("accepts"),
                Span::styled("a compound value — see the format below", Style::new().fg(MUTED)),
            ])),
            _ => {}
        }
        if key == "theme" && !self.themes.is_empty() {
            lines.push(Line::from(vec![
                label("themes"),
                Span::styled(format!("{} installed — ⏎ to preview", self.themes.len()), Style::new().fg(MUTED)),
            ]));
        }
        if key.contains("font-family") && !self.fonts.is_empty() {
            lines.push(Line::from(vec![
                label("fonts"),
                Span::styled(format!("{} families installed — ⏎ to pick", self.fonts.len()), Style::new().fg(MUTED)),
            ]));
        }
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.extend(reflow(&setting.doc).into_iter().map(Line::raw));

        // `keybind` is the one key whose value space lives in a separate query.
        // Scrolled to at the bottom of its own docs is where it is wanted.
        if key == "keybind" && !self.actions.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("── {} actions a keybind can trigger ──", self.actions.len()),
                Style::new().fg(MUTED),
            ));
            lines.extend(
                self.actions
                    .iter()
                    .map(|a| Line::styled(format!("  {a}"), Style::new().fg(Color::Green))),
            );
        }
        Text::from(lines)
    }

    fn draw_edit(&self, f: &mut Frame, key: &str, area_widget: &TextArea<'static>) {
        let area = popup(f.area(), 72, 5);
        f.render_widget(Clear, area);
        f.render_widget(pane(&format!(" {key} ")), area);

        let inner = area.inner(Margin { vertical: 1, horizontal: 2 });
        let [input, _, hint] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);

        f.render_widget(area_widget, input);
        // No "accepts" line here: a key that reaches the text editor is one
        // Ghostty describes only in prose, and that prose is already in the
        // pane behind this popup.
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("{}   ⏎ apply   ^U clear   esc cancel", self.format_hint(key)),
                Style::new().fg(MUTED),
            )),
            hint,
        );
    }

    /// One line naming the shape of the value, for the keys with a grammar.
    fn format_hint(&self, key: &str) -> String {
        match self.kind(key) {
            Kind::Structured => "compound value".into(),
            Kind::Scalar => "single value".into(),
            _ => "free text".into(),
        }
    }

    /// The picker: a filtered list, plus a preview of what the highlighted value
    /// actually looks like when that is a thing you can see.
    fn draw_pick(&self, f: &mut Frame, p: &Picker) {
        let visible = p.visible();
        let wide = p.preview != Preview::None;
        // A four-value enum in a 22-row box reads as a list that failed to load.
        let height = match wide {
            true => 22,
            false => (p.items.len() as u16 + 2).clamp(5, 22),
        };
        let area = popup(f.area(), if wide { 96 } else { 62 }, height);
        f.render_widget(Clear, area);

        let title = match p.filter.is_empty() {
            true => format!(" {}  ·  {} ", p.key, visible.len()),
            false => format!(" {}  /{}▏ {} ", p.key, p.filter, visible.len()),
        };
        let block = pane(&title).title_bottom(Span::styled(
            if p.free {
                " ↑↓ move · type to filter or enter a value · ⏎ apply · esc "
            } else {
                " ↑↓ move · type to filter · ⏎ apply · esc "
            },
            Style::new().fg(MUTED),
        ));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let (column, prev_area) = if wide {
            let [l, r] = Layout::horizontal([Constraint::Length(36), Constraint::Min(0)])
                .spacing(2)
                .areas(inner);
            (l, Some(r))
        } else {
            (inner, None)
        };
        // The last column of the list belongs to its scrollbar, so a long theme
        // name is truncated rather than drawn under the thumb.
        let list_area = Rect { width: column.width.saturating_sub(1), ..column };

        let items: Vec<ListItem> = visible
            .iter()
            .map(|v| {
                let mut spans = vec![Span::styled(
                    if **v == p.current { "● " } else { "  " },
                    Style::new().fg(Color::Yellow),
                )];
                if p.preview == Preview::Color {
                    if let Some(c) = self.hex_of(v).as_deref().and_then(parse_hex) {
                        spans.push(Span::styled("██ ", Style::new().fg(c)));
                    }
                }
                spans.push(Span::raw((*v).clone()));
                // The plain "… Nerd Font" build draws icons at double cell width
                // and pushes prompts out of alignment; the "Mono" build does not.
                if v.ends_with("Nerd Font") {
                    spans.push(Span::styled(" ⚠", Style::new().fg(Color::Yellow)));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let mut state = p.state;
        f.render_stateful_widget(
            List::new(items).highlight_style(Style::new().fg(Color::Black).bg(ACCENT)),
            list_area,
            &mut state,
        );
        bar(f, column, visible.len(), state.selected().unwrap_or(0));

        let Some(prev_area) = prev_area else { return };
        let sel = state.selected().and_then(|i| visible.get(i)).map(|s| (*s).clone());
        let Some(sel) = sel else { return };
        match p.preview {
            Preview::Theme => self.draw_theme_preview(f, prev_area, &sel),
            Preview::Font => self.draw_font_preview(f, prev_area, &sel),
            Preview::Color => self.draw_color_preview(f, prev_area, &sel),
            Preview::None => {}
        }
    }

    /// A theme is a config file of colours, so it can be shown before it is
    /// applied — which beats applying 463 of them one at a time to find out.
    fn draw_theme_preview(&self, f: &mut Frame, area: Rect, name: &str) {
        let Some(gh) = &self.gh else { return };
        let Some(entries) = gh.theme_colors(name) else {
            f.render_widget(
                Paragraph::new(Line::styled("no colour file for this theme", Style::new().fg(MUTED))),
                area,
            );
            return;
        };
        let find = |k: &str| {
            entries
                .iter()
                .find(|(ek, _)| ek == k)
                .and_then(|(_, v)| parse_hex(v))
        };
        let mut palette: Vec<(usize, Color)> = entries
            .iter()
            .filter(|(k, _)| k == "palette")
            .filter_map(|(_, v)| {
                let (i, hex) = v.split_once('=')?;
                Some((i.trim().parse::<usize>().ok()?, parse_hex(hex.trim())?))
            })
            .collect();
        palette.sort_by_key(|(i, _)| *i);

        let bg = find("background").unwrap_or(Color::Reset);
        let fg = find("foreground").unwrap_or(Color::Reset);
        let swatches = |from: usize| -> Line<'static> {
            Line::from(
                palette
                    .iter()
                    .filter(|(i, _)| (from..from + 8).contains(i))
                    .map(|(_, c)| Span::styled("███", Style::new().fg(*c)))
                    .collect::<Vec<_>>(),
            )
        };

        let [head, sample, _] =
            Layout::vertical([Constraint::Length(7), Constraint::Length(6), Constraint::Min(0)])
                .areas(area);
        let mut lines = vec![
            Line::styled(name.to_string(), Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
            Line::raw(""),
            Line::from(vec![
                Span::styled("normal  ", Style::new().fg(MUTED)),
            ]),
        ];
        lines.push(swatches(0));
        lines.push(Line::styled("bright  ", Style::new().fg(MUTED)));
        lines.push(swatches(8));
        f.render_widget(Paragraph::new(lines), head);

        // Rendered in the theme's own background and foreground: this is the
        // question people actually have — is it readable.
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::from(vec![
                    Span::raw("  $ "),
                    Span::styled("ghostty", Style::new().fg(palette.get(4).map_or(fg, |(_, c)| *c))),
                    Span::raw(" --config "),
                    Span::styled("~/.config", Style::new().fg(palette.get(2).map_or(fg, |(_, c)| *c))),
                ]),
                Line::raw("  the quick brown fox jumps over"),
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled("error:", Style::new().fg(palette.first().map_or(fg, |(_, c)| *c))),
                    Span::raw(" nothing is wrong"),
                ]),
                Line::raw(""),
            ])
            .style(Style::new().bg(bg).fg(fg)),
            sample,
        );
    }

    fn draw_font_preview(&self, f: &mut Frame, area: Rect, name: &str) {
        let styles = self
            .fonts
            .iter()
            .find(|ff| ff.name == *name)
            .map(|ff| ff.styles.clone())
            .unwrap_or_default();
        let mut lines = vec![
            Line::styled(name.to_string(), Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
            Line::raw(""),
            Line::styled(format!("{} styles installed", styles.len()), Style::new().fg(MUTED)),
        ];
        lines.extend(
            styles
                .iter()
                .map(|s| Line::styled(format!("  {s}"), Style::new().fg(Color::Green))),
        );
        if name.ends_with("Nerd Font") {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "⚠ this build draws icons at double cell width and",
                Style::new().fg(Color::Yellow),
            ));
            lines.push(Line::styled(
                "  pushes prompts out of alignment — prefer the",
                Style::new().fg(Color::Yellow),
            ));
            lines.push(Line::styled("  \"… Nerd Font Mono\" build.", Style::new().fg(Color::Yellow)));
        }
        // No sample here on purpose: this terminal draws in the font it was
        // launched with, so any "preview" would be a lie. Applying it asks
        // Ghostty whether the family really renders, which is the real answer.
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "⏎ checks with Ghostty that this family renders",
            Style::new().fg(MUTED),
        ));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn draw_color_preview(&self, f: &mut Frame, area: Rect, name: &str) {
        let hex = self.hex_of(name).unwrap_or_default();
        let color = parse_hex(&hex).unwrap_or(Color::Reset);
        let [head, block] =
            Layout::vertical([Constraint::Length(4), Constraint::Min(0)]).areas(area);
        f.render_widget(
            Paragraph::new(vec![
                Line::styled(name.to_string(), Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
                Line::styled(hex, Style::new().fg(MUTED)),
                Line::raw(""),
            ]),
            head,
        );
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::styled("  the quick brown fox  ", Style::new().fg(Color::Black)),
                Line::styled("  the quick brown fox  ", Style::new().fg(Color::White)),
                Line::raw(""),
            ])
            .style(Style::new().bg(color)),
            block,
        );
    }

    fn draw_checks(&mut self, f: &mut Frame, left: Rect, right: Rect) {
        let Some(checks) = &self.checks else {
            f.render_widget(
                Paragraph::new(Line::styled(" running checks…", Style::new().fg(MUTED)))
                    .block(pane(" Check ")),
                left.union(right),
            );
            return;
        };
        let failed = checks.iter().filter(|c| !c.ok).count();
        let items: Vec<ListItem> = checks
            .iter()
            .map(|c| {
                let (mark, style) = if c.ok {
                    ("✓ ", Style::new().green())
                } else {
                    ("✗ ", Style::new().red())
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {mark}"), style),
                    Span::raw(c.title.clone()),
                ]))
            })
            .collect();
        let title = if failed == 0 {
            format!(" {} checks, all passing ", checks.len())
        } else {
            format!(" {failed} of {} failing ", checks.len())
        };
        f.render_stateful_widget(
            List::new(items)
                .block(pane(&title))
                .highlight_style(Style::new().fg(Color::Black).bg(ACCENT)),
            left,
            &mut self.list,
        );
        scrollbar(f, left, checks.len(), self.list.selected().unwrap_or(0));

        let detail = checks
            .get(self.list.selected().unwrap_or(0))
            .map(|c| {
                let mut lines = vec![Line::raw(c.title.clone()), Line::raw("")];
                lines.extend(c.detail.iter().map(|d| Line::styled(d.clone(), Style::new().fg(MUTED))));
                Text::from(lines)
            })
            .unwrap_or_default();
        f.render_widget(
            Paragraph::new(detail).block(pane(" Detail ")).wrap(Wrap { trim: false }),
            right,
        );
    }

    fn draw_help(&self, f: &mut Frame) {
        const HELP: &[(&str, &str)] = &[
            ("j / k  ↓ ↑", "move"),
            ("^D / ^U", "half page"),
            ("g / G", "first / last"),
            ("h / l  ⇥", "switch tab"),
            ("/", "search (fuzzy on the key name)"),
            ("⏎", "edit — a list where Ghostty knows the values"),
            ("d", "unset, back to Ghostty's default"),
            ("s", "save now (validated before it is written)"),
            ("PgUp / PgDn", "scroll the docs pane"),
            ("R", "drop the font/theme/schema caches"),
            ("q / Q", "quit applying / quit discarding"),
        ];
        let area = popup(f.area(), 64, HELP.len() as u16 + 2);
        f.render_widget(Clear, area);
        let lines: Vec<Line> = HELP
            .iter()
            .map(|(k, v)| {
                Line::from(vec![
                    Span::styled(format!(" {k:<12}"), Style::new().fg(ACCENT)),
                    Span::styled((*v).to_string(), Style::new().fg(Color::White)),
                ])
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).block(
                pane(" keys ").title_bottom(Span::styled(" any key to close ", Style::new().fg(MUTED))),
            ),
            area,
        );
    }

    fn footer(&self) -> Line<'static> {
        let keys: &[(&str, &str)] = match self.mode {
            Mode::Help => &[("any key", "close")],
            Mode::Search => &[("esc", "clear"), ("⏎", "keep filter"), ("^U", "wipe")],
            Mode::Edit { .. } => &[("⏎", "apply"), ("^U", "clear"), ("esc", "cancel")],
            Mode::Pick(_) => &[("↑↓", "move"), ("type", "filter"), ("⏎", "apply"), ("esc", "cancel")],
            Mode::Browse if self.tab == 2 => &[("⇥", "tab"), ("r", "re-run"), ("?", "keys"), ("q", "quit")],
            Mode::Browse => &[
                ("⇥", "tab"),
                ("/", "search"),
                ("⏎", "edit"),
                ("d", "unset"),
                ("s", "save"),
                ("?", "keys"),
                ("q", "apply & quit"),
            ],
        };
        let mut spans = vec![Span::raw(" ")];
        for (k, label) in keys {
            spans.push(Span::styled(format!(" {k}"), Style::new().fg(ACCENT)));
            spans.push(Span::styled(format!(" {label} "), Style::new().fg(MUTED)));
        }
        Line::from(spans)
    }
}

impl ConfigFile {
    fn positions_count(&self) -> usize {
        self.entries.iter().filter(|e| matches!(e, Entry::Setting { .. })).count()
    }
}

/// Subsequence match, scored so that a prefix beats a match buried in the middle
/// and a run of adjacent characters beats scattered ones. `wpx` finds
/// `window-padding-x`, which a substring filter cannot.
fn fuzzy(haystack: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let mut score = 0;
    let mut at = 0;
    let mut run = 0;
    for c in needle.to_lowercase().chars() {
        let found = hay[at..].iter().position(|h| *h == c)? + at;
        // A match at the start of the string or just after a separator is what
        // an abbreviation like `wpx` means.
        let boundary = found == 0 || matches!(hay[found - 1], '-' | '_' | '.' | ' ');
        score += 1 + run * 4 + if boundary { 6 } else { 0 } - (found - at).min(6) as i32;
        run = if found == at { run + 1 } else { 0 };
        at = found + 1;
    }
    Some(score)
}

/// `#rrggbb`, with or without the hash. Anything else is not our business to
/// guess at — the swatch is simply omitted.
fn parse_hex(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn new_textarea(value: &str) -> TextArea<'static> {
    let mut a = TextArea::new(vec![value.to_string()]);
    a.move_cursor(ratatui_textarea::CursorMove::End);
    a.set_cursor_line_style(Style::default());
    a
}

/// A slim scrollbar for a bordered pane. Drawn only when there is more content
/// than room, so short lists stay uncluttered.
fn scrollbar(f: &mut Frame, area: Rect, len: usize, pos: usize) {
    // The border eats a row top and bottom, and the bar is drawn between them.
    bar(f, area.inner(Margin { vertical: 1, horizontal: 0 }), len, pos);
}

/// The same bar for an area that has no border of its own.
fn bar(f: &mut Frame, area: Rect, len: usize, pos: usize) {
    let rows = area.height as usize;
    if len <= rows {
        return;
    }
    let mut state = ScrollbarState::new(len - rows).position(pos);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("▐")
            .thumb_style(Style::new().fg(MUTED)),
        area,
        &mut state,
    );
}

fn pane(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(MUTED))
        .padding(Padding::horizontal(1))
        .title(Span::styled(title.to_string(), Style::new().fg(Color::White)))
}

fn popup(area: Rect, w: u16, h: u16) -> Rect {
    let [a] = Layout::horizontal([Constraint::Length(w.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [a] = Layout::vertical([Constraint::Length(h.min(area.height))])
        .flex(Flex::Center)
        .areas(a);
    a
}

/// Undo Ghostty's hard wrapping so the pane can wrap to its own width.
///
/// The docs arrive wrapped at ~76 columns. Re-wrapping already-wrapped text in
/// a narrower pane is what produces those two-word orphan lines, so paragraphs
/// are rejoined first and bullets/indented samples are left alone.
fn reflow(doc: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut para = String::new();
    let flush = |para: &mut String, out: &mut Vec<String>| {
        if !para.is_empty() {
            out.push(std::mem::take(para));
        }
    };
    for line in doc.lines() {
        let t = line.trim();
        if t.is_empty() {
            flush(&mut para, &mut out);
            out.push(String::new());
        } else if t.starts_with('*') || t.starts_with('-') {
            // A new bullet; its continuation lines join it below.
            flush(&mut para, &mut out);
            para = format!("  {t}");
        } else if para.is_empty() && line.starts_with("    ") {
            // An indented sample, standing alone between blank lines.
            out.push(line.to_string());
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(t);
        }
    }
    flush(&mut para, &mut out);
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

fn pretty_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    let home = crate::ghostty::home().display().to_string();
    if !home.is_empty() && s.starts_with(&home) {
        return s.replacen(&home, "~", 1);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_an_abbreviation_and_ranks_the_obvious_key_first() {
        assert!(fuzzy("window-padding-x", "wpx").is_some());
        assert!(fuzzy("window-padding-x", "zzz").is_none());
        // The whole point of the score: typing `font` must rank the key that
        // starts with it above one that merely contains it further along.
        let direct = fuzzy("font-family", "font").unwrap();
        let buried = fuzzy("window-title-font-family", "font").unwrap();
        assert!(direct > buried, "{direct} !> {buried}");

        // And an abbreviation must beat a scattered coincidence.
        let abbrev = fuzzy("window-padding-x", "wpx").unwrap();
        let coincidence = fuzzy("wait-after-command-plus-exit", "wpx").unwrap();
        assert!(abbrev > coincidence, "{abbrev} !> {coincidence}");
    }

    #[test]
    fn hex_parses_with_and_without_the_hash() {
        assert_eq!(parse_hex("#2e3440"), Some(Color::Rgb(0x2e, 0x34, 0x40)));
        assert_eq!(parse_hex("2e3440"), Some(Color::Rgb(0x2e, 0x34, 0x40)));
        assert_eq!(parse_hex("nope"), None);
    }

    fn rows(n: usize) -> Vec<Row> {
        let mut v = vec![Row::Header("h".into())];
        v.extend((0..n).map(|i| Row::Setting {
            key: format!("k{i}"),
            value: String::new(),
            idx: None,
            pickable: false,
        }));
        v.push(Row::Header("h2".into()));
        v.push(Row::Setting { key: "last".into(), value: String::new(), idx: None, pickable: false });
        v
    }

    #[test]
    fn the_cursor_never_lands_on_a_header() {
        let mut app = App::new(None, ConfigFile { path: Default::default(), entries: vec![], dirty: false });
        let rows = rows(2);
        for start in 0..rows.len() {
            for delta in [-1isize, 1, -10, 10] {
                app.list.select(Some(start));
                app.move_sel(delta, &rows);
                let i = app.list.selected().unwrap();
                assert!(
                    !matches!(rows[i], Row::Header(_)),
                    "landed on header at {i} from {start} by {delta}"
                );
            }
        }
    }

    /// The pathological input for the header-skipping loop, which runs inside
    /// the draw path: nothing to land on, so it must give up rather than spin.
    #[test]
    fn all_headers_terminates() {
        let mut app = App::new(None, ConfigFile { path: Default::default(), entries: vec![], dirty: false });
        let rows = vec![Row::Header("a".into()), Row::Header("b".into())];
        app.move_sel(1, &rows);
        app.move_sel(-1, &rows);
    }

    #[test]
    fn opens_on_the_all_tab() {
        let app = App::new(None, ConfigFile { path: Default::default(), entries: vec![], dirty: false });
        assert_eq!(app.tab, 0);
        assert_eq!(TABS[0], "All");
    }
}
