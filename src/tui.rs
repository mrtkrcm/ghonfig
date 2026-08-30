//! The interactive configuration browser.
//!
//! Two panes: the settings on the left, Ghostty's own documentation for the
//! selected one on the right. That is the whole idea — the docs for these 634
//! keys otherwise exist only inside the binary, so browsing them beside your
//! actual values is most of the value the tool has to offer.

use crate::check::{self, Check};
use crate::config::{ConfigFile, Entry};
use crate::ghostty::{FontStatus, Ghostty, Schema};
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
use std::time::Duration;
use throbber_widgets_tui::{Throbber, ThrobberState, BRAILLE_SIX};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;

const TABS: [&str; 3] = ["Set", "All", "Check"];

enum Loaded {
    Schema(Schema),
    Themes(Vec<String>),
    Fonts(Vec<String>),
}

enum Mode {
    Browse,
    Search,
    /// A real editor rather than an append-only buffer: half these values are
    /// long comma lists (`shell-integration-features`) where retyping from
    /// scratch to change one word is the whole friction.
    Edit { key: String, area: Box<TextArea<'static>> },
    Pick { key: String, items: Vec<String>, filter: String, state: ListState },
}

struct Row {
    key: String,
    value: String,
    /// Index into the config file, when the key is actually assigned there.
    idx: Option<usize>,
}

pub struct App {
    gh: Option<Ghostty>,
    cfg: ConfigFile,
    schema: Option<Schema>,
    themes: Vec<String>,
    fonts: Vec<String>,
    pending: usize,
    rx: Receiver<Loaded>,
    tab: usize,
    list: ListState,
    filter: String,
    mode: Mode,
    toast: Option<(String, bool)>,
    checks: Option<Vec<Check>>,
    check_rx: Option<Receiver<Vec<Check>>>,
    /// Valid values for the key being edited, once Ghostty has been asked.
    hint: Option<String>,
    hint_values: Option<Vec<String>>,
    hint_rx: Option<Receiver<Option<Vec<String>>>>,
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
            pending = 3;
            std::thread::spawn(move || {
                if let Ok(s) = g.schema() {
                    let _ = tx.send(Loaded::Schema(Schema::new(s)));
                }
                if let Ok(t) = g.themes() {
                    let _ = tx.send(Loaded::Themes(t));
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
            pending,
            rx,
            tab: 0,
            list,
            filter: String::new(),
            mode: Mode::Browse,
            toast: None,
            checks: None,
            check_rx: None,
            hint: None,
            hint_values: None,
            hint_rx: None,
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
                Loaded::Schema(s) => self.schema = Some(s),
                Loaded::Themes(t) => self.themes = t,
                Loaded::Fonts(f) => self.fonts = f,
            }
        }
        if let Some(rx) = &self.check_rx {
            if let Ok(c) = rx.try_recv() {
                self.checks = Some(c);
                self.check_rx = None;
            }
        }
        if let Some(rx) = &self.hint_rx {
            if let Ok(v) = rx.try_recv() {
                self.hint = v
                    .as_ref()
                    .map(|vals| format!("⇥ cycle   accepts: {}", vals.join(", ")));
                self.hint_values = v;
                self.hint_rx = None;
            }
        }
    }

    fn repeatable(&self, key: &str) -> bool {
        self.schema.as_ref().is_some_and(|s| s.is_repeatable(key))
    }

    // ── rows ────────────────────────────────────────────────────────────────

    fn rows(&self) -> Vec<Row> {
        let f = self.filter.to_lowercase();
        let keep = |k: &str, v: &str| {
            f.is_empty() || k.to_lowercase().contains(&f) || v.to_lowercase().contains(&f)
        };
        match self.tab {
            // Only what this config actually assigns — one row per line, so a
            // key assigned twice is visibly assigned twice.
            0 => self
                .cfg
                .entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| match e {
                    Entry::Setting { key, value } if keep(key, value) => Some(Row {
                        key: key.clone(),
                        value: value.clone(),
                        idx: Some(i),
                    }),
                    _ => None,
                })
                .collect(),
            // Every key Ghostty has, with your value if you set one.
            1 => self
                .schema
                .as_ref()
                .map(|s| {
                    s.settings
                        .iter()
                        .filter_map(|st| {
                            let idx = self.cfg.effective(&st.key, self.repeatable(&st.key));
                            let value = idx
                                .map(|i| self.cfg.value_at(i).to_string())
                                .unwrap_or_else(|| st.default_display());
                            keep(&st.key, &value).then_some(Row {
                                key: st.key.clone(),
                                value,
                                idx,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => vec![],
        }
    }

    fn selected_row(&self) -> Option<Row> {
        let rows = self.rows();
        self.list.selected().and_then(|i| rows.into_iter().nth(i))
    }

    // ── actions ─────────────────────────────────────────────────────────────

    fn open_editor(&mut self) {
        let Some(row) = self.selected_row() else { return };
        let key = row.key.clone();
        let current = row.idx.map(|i| self.cfg.value_at(i).to_string()).unwrap_or_default();

        // The three keys worth a real picker are the three with hundreds of
        // valid values that Ghostty can enumerate.
        let items = match key.as_str() {
            "theme" => self.themes.clone(),
            k if k.starts_with("font-family") => self.fonts.clone(),
            k => self.schema.as_ref().and_then(|s| s.choices(k)).unwrap_or_default(),
        };
        if !items.is_empty() {
            let mut state = ListState::default();
            state.select(Some(items.iter().position(|i| *i == current).unwrap_or(0)));
            self.mode = Mode::Pick { key, items, filter: String::new(), state };
            return;
        }

        // Otherwise a free-text field. Ghostty is asked in the background what
        // this key accepts; if it is an enum, the answer becomes a hint.
        self.hint = None;
        self.hint_values = None;
        if let Some(gh) = self.gh.clone() {
            let (tx, rx) = channel();
            let k = key.clone();
            std::thread::spawn(move || {
                let _ = tx.send(gh.valid_values(&k));
            });
            self.hint_rx = Some(rx);
        }
        self.mode = Mode::Edit { key, area: Box::new(new_textarea(&current)) };
    }

    fn apply(&mut self, key: &str, value: &str) {
        let repeatable = self.repeatable(key);
        // Edit the exact line the cursor is on when there is one, so editing the
        // second of two assignments does not silently rewrite the first.
        match self.selected_row().and_then(|r| (r.key == key).then_some(r.idx).flatten()) {
            Some(i) => self.cfg.set_at(i, value),
            None => self.cfg.set(key, value, repeatable),
        }

        // A font Ghostty cannot render is accepted silently and falls back, so
        // this is the one moment the mistake is visible.
        if key.starts_with("font-family") && !value.is_empty() {
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
        self.toast(format!("{key} = {value}   (s to save)"), false);
    }

    fn unset(&mut self) {
        let Some(row) = self.selected_row() else { return };
        match row.idx {
            Some(i) => {
                self.cfg.remove_at(i);
                self.toast(format!("unset {} — back to Ghostty's default   (s to save)", row.key), false);
            }
            None => self.toast(format!("{} is not set here", row.key), false),
        }
    }

    fn save(&mut self) {
        match self.cfg.save(self.gh.as_ref()) {
            Ok(()) => self.toast("saved — reload Ghostty with Cmd+Shift+,".into(), false),
            Err(e) => self.toast(e.to_string().lines().next().unwrap_or("invalid").to_string(), true),
        }
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
        self.toast = Some((msg, err));
    }

    fn move_sel(&mut self, delta: isize, len: usize) {
        // A new selection means new documentation, so the docs pane goes back
        // to the top rather than showing the middle of an unrelated key's text.
        self.doc_scroll = 0;
        if len == 0 {
            self.list.select(Some(0));
            return;
        }
        let cur = self.list.selected().unwrap_or(0) as isize;
        self.list.select(Some((cur + delta).clamp(0, len as isize - 1) as usize));
    }

    // ── input ───────────────────────────────────────────────────────────────

    fn key(&mut self, k: KeyEvent) {
        match &mut self.mode {
            Mode::Browse => self.key_browse(k),
            Mode::Search => match k.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.mode = Mode::Browse;
                }
                KeyCode::Enter => self.mode = Mode::Browse,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.list.select(Some(0));
                }
                _ => {}
            },
            Mode::Edit { key, area } => match k.code {
                KeyCode::Esc => self.mode = Mode::Browse,
                // The editor opens with the cursor at the end of the existing
                // value, so typing appends. ratatui-textarea has no ^U of its
                // own and that is the reflex for "clear this field".
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    *area = Box::new(new_textarea(""));
                }
                // Once Ghostty has told us the closed set, stepping through it
                // beats typing one of its members by hand.
                KeyCode::Tab => {
                    if let Some(vals) = &self.hint_values {
                        let cur = area.lines().join("");
                        let next = vals
                            .iter()
                            .position(|v| *v == cur)
                            .map(|i| (i + 1) % vals.len())
                            .unwrap_or(0);
                        *area = Box::new(new_textarea(&vals[next]));
                    }
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
            Mode::Pick { key, items, filter, state } => {
                let visible: Vec<String> = items
                    .iter()
                    .filter(|i| i.to_lowercase().contains(&filter.to_lowercase()))
                    .cloned()
                    .collect();
                match k.code {
                    KeyCode::Esc => self.mode = Mode::Browse,
                    KeyCode::Up => {
                        let i = state.selected().unwrap_or(0);
                        state.select(Some(i.saturating_sub(1)));
                    }
                    KeyCode::Down => {
                        let i = state.selected().unwrap_or(0) + 1;
                        state.select(Some(i.min(visible.len().saturating_sub(1))));
                    }
                    KeyCode::Enter => {
                        let pick = state.selected().and_then(|i| visible.get(i)).cloned();
                        let key = key.clone();
                        self.mode = Mode::Browse;
                        if let Some(v) = pick {
                            self.apply(&key, &v);
                        }
                    }
                    KeyCode::Backspace => {
                        filter.pop();
                        state.select(Some(0));
                    }
                    KeyCode::Char(c) => {
                        filter.push(c);
                        state.select(Some(0));
                    }
                    _ => {}
                }
            }
        }
    }

    fn key_browse(&mut self, k: KeyEvent) {
        let len = if self.tab == 2 {
            self.checks.as_ref().map_or(0, |c| c.len())
        } else {
            self.rows().len()
        };
        match k.code {
            KeyCode::Char('q') => {
                if self.cfg.dirty {
                    self.toast("unsaved changes — s to save, Q to discard".into(), true);
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Char('Q') => self.quit = true,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.tab = (self.tab + 1) % TABS.len();
                self.list.select(Some(0));
                if self.tab == 2 {
                    self.run_checks();
                }
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.tab = (self.tab + TABS.len() - 1) % TABS.len();
                self.list.select(Some(0));
                if self.tab == 2 {
                    self.run_checks();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1, len),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1, len),
            // The docs pane scrolls on its own keys: `keybind` alone documents
            // 93 defaults, far more than fits beside the list.
            KeyCode::PageDown => {
                self.doc_scroll = (self.doc_scroll + 8).min(self.doc_lines as u16)
            }
            KeyCode::PageUp => self.doc_scroll = self.doc_scroll.saturating_sub(8),
            KeyCode::Home | KeyCode::Char('g') => {
                self.list.select(Some(0));
                self.doc_scroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') => self.move_sel(len as isize, len),
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
            KeyCode::Char('s') => self.save(),
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

        if let Some((msg, err)) = &self.toast {
            let style = if *err { Style::new().red() } else { Style::new().fg(ACCENT) };
            f.render_widget(
                Paragraph::new(Line::from(format!(" {msg}")).style(style)),
                toast,
            );
        }
        f.render_widget(Paragraph::new(self.footer()), footer);
    }

    fn draw_header(&mut self, f: &mut Frame, area: Rect) {
        let [title_row, tab_row, _] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled("ghonfig", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
            ])),
            title_row,
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(pretty_path(&self.cfg.path), Style::new().fg(MUTED)),
                Span::styled(
                    if self.cfg.dirty { " ●" } else { "  " },
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
                0 => format!("{t} {}", self.cfg.positions_count()),
                1 => match &self.schema {
                    Some(s) => format!("{t} {}", s.settings.len()),
                    None => t.to_string(),
                },
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
        let keyw = width.min(28).max(12);

        let items: Vec<ListItem> = rows
            .iter()
            .map(|r| {
                let value = truncate(&r.value, width.saturating_sub(keyw + 1));
                let vstyle = if r.idx.is_some() {
                    Style::new().fg(Color::White)
                } else {
                    // Showing Ghostty's default, not your value.
                    Style::new().fg(MUTED).add_modifier(Modifier::ITALIC)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<keyw$} ", truncate(&r.key, keyw)), Style::new().fg(ACCENT)),
                    Span::styled(value, vstyle),
                ]))
            })
            .collect();

        let title = if matches!(self.mode, Mode::Search) || !self.filter.is_empty() {
            format!(" /{}", self.filter)
        } else {
            format!(" {} ", rows.len())
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

        // Right pane: Ghostty's documentation for the selected key.
        let (title, text) = match rows.get(self.list.selected().unwrap_or(0)) {
            Some(row) => {
                let mut lines = vec![];
                if let Some(s) = self.schema.as_ref().and_then(|s| s.get(&row.key)) {
                    let d = s.default_display();
                    if !d.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled("default  ", Style::new().fg(MUTED)),
                            Span::raw(d),
                        ]));
                        lines.push(Line::raw(""));
                    }
                    lines.extend(reflow(&s.doc).into_iter().map(Line::raw));
                } else if self.pending > 0 {
                    lines.push(Line::styled("loading Ghostty's docs…", Style::new().fg(MUTED)));
                } else {
                    lines.push(Line::styled(
                        "No documentation — Ghostty does not know this key.",
                        Style::new().red(),
                    ));
                }
                (format!(" {} ", row.key), Text::from(lines))
            }
            None => (" ".into(), Text::raw("")),
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
            Mode::Pick { key, items, filter, .. } => {
                let visible: Vec<String> = items
                    .iter()
                    .filter(|i| i.to_lowercase().contains(&filter.to_lowercase()))
                    .cloned()
                    .collect();
                self.draw_pick(f, key, filter, visible);
            }
            _ => {}
        }
    }

    fn draw_edit(&self, f: &mut Frame, key: &str, area_widget: &TextArea<'static>) {
        let area = popup(f.area(), 72, 5);
        f.render_widget(Clear, area);
        f.render_widget(pane(&format!(" {key} ")), area);

        let inner = area.inner(Margin { vertical: 1, horizontal: 2 });
        let [input, _, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

        f.render_widget(area_widget, input);
        let hint_text = self
            .hint
            .clone()
            .unwrap_or_else(|| "⏎ apply   ^U clear   esc cancel".into());
        f.render_widget(
            Paragraph::new(Line::styled(hint_text, Style::new().fg(MUTED))).wrap(Wrap { trim: true }),
            hint,
        );
    }

    fn draw_pick(&self, f: &mut Frame, key: &str, filter: &str, visible: Vec<String>) {
        let area = popup(f.area(), 60, 18);
        f.render_widget(Clear, area);
        let mut state = ListState::default();
        if let Mode::Pick { state: s, .. } = &self.mode {
            state = s.clone();
        }
        let items: Vec<ListItem> = visible
            .iter()
            .map(|v| {
                // The plain "… Nerd Font" build draws icons at double cell width
                // and pushes prompts out of alignment; the "Mono" build does not.
                let warn = v.ends_with("Nerd Font");
                let mut spans = vec![Span::raw(format!(" {v}"))];
                if warn {
                    spans.push(Span::styled(
                        "  ⚠ double-width icons — prefer the Mono build",
                        Style::new().fg(Color::Yellow),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let title = if filter.is_empty() {
            format!(" {key}  ·  {} ", visible.len())
        } else {
            format!(" {key}  /{filter} ")
        };
        f.render_stateful_widget(
            List::new(items)
                .block(pane(&title))
                .highlight_style(Style::new().fg(Color::Black).bg(ACCENT)),
            area,
            &mut state,
        );
        scrollbar(f, area, visible.len(), state.selected().unwrap_or(0));
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

    fn footer(&self) -> Line<'static> {
        let keys: &[(&str, &str)] = match self.mode {
            Mode::Search => &[("esc", "clear"), ("⏎", "keep filter")],
            Mode::Edit { .. } => &[("⏎", "apply"), ("esc", "cancel")],
            Mode::Pick { .. } => &[("↑↓", "move"), ("type", "filter"), ("⏎", "apply"), ("esc", "cancel")],
            Mode::Browse if self.tab == 2 => &[("⇥", "tab"), ("r", "re-run"), ("q", "quit")],
            Mode::Browse => &[
                ("⇥", "tab"),
                ("/", "search"),
                ("⏎", "edit"),
                ("d", "unset"),
                ("s", "save"),
                ("q", "quit"),
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

fn new_textarea(value: &str) -> TextArea<'static> {
    let mut a = TextArea::new(vec![value.to_string()]);
    a.move_cursor(ratatui_textarea::CursorMove::End);
    a.set_cursor_line_style(Style::default());
    a
}

/// A slim scrollbar inside a bordered pane. Drawn only when there is more
/// content than room, so short lists stay uncluttered.
fn scrollbar(f: &mut Frame, area: Rect, len: usize, pos: usize) {
    let rows = area.height.saturating_sub(2) as usize;
    if len <= rows {
        return;
    }
    let mut state = ScrollbarState::new(len.saturating_sub(rows)).position(pos);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("▐")
            .thumb_style(Style::new().fg(MUTED)),
        area.inner(Margin { vertical: 1, horizontal: 0 }),
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
