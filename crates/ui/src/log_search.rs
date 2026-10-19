// SPDX-License-Identifier: GPL-3.0-only
//! The log search bar (#404): incremental search over the logs the
//! application holds in memory.
//!
//! The data walk is `cayenchat_app::search`; this module owns what the user
//! sees and does: the bar above the main log, the keys, the scan being
//! driven in slices from the executor (never one long stretch on the UI
//! thread), and moving to a hit, in another conversation if need be. A hit is
//! a log and a message sequence, so lines that arrive, are trimmed or are
//! inserted above cannot make it point at the wrong line.

use std::{cell::Cell, rc::Rc, time::Duration};

use cayenchat_app::{
    Command, Selection,
    search::{self, Hit, Query, Scan, Scope, Source},
};
use gpui::{prelude::*, *};

use crate::{
    ChatWindow, CloseSearch, CompleteNickname, FindInLog, HistoryNext, HistoryPrevious, Notice,
    SearchNext, SearchPrevious, SendMessage, input::TextInput, ircv3_settings::TextTooltip,
    theme::Theme, yield_now,
};

/// Height of the bar, including its bottom border.
pub const BAR_HEIGHT: f32 = 36.;
/// Pause after the last change of the term before the logs are read.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// Pause before the logs are read again after they changed, so a busy
/// channel costs a few reads a second at most.
const RESCAN: Duration = Duration::from_millis(500);
/// How long the bar takes to slide into place.
const SLIDE: Duration = Duration::from_millis(120);
/// Budget per slice (see `Scan::step`), so one slice stays a few
/// milliseconds: 20,000 short messages or about 1.3 MB of text.
const SLICE: usize = 20_000;

/// Search state of one chat window. Created when the bar is first opened.
pub struct LogSearch {
    input: Entity<TextInput>,
    open: bool,
    scope: Scope,
    /// The committed term (what is in the field, once any composition is
    /// finished) and its matcher; `None` for an empty field.
    term: String,
    query: Option<Query>,
    /// The search shown: its hits are what the count and the arrows use, from
    /// the moment it is done until a newer one replaces it.
    scan: Option<Scan>,
    /// A second read of the logs after they changed. It takes the place of
    /// `scan` when done, so lines arriving all the time never leave the
    /// count unfinished.
    pending: Option<Scan>,
    /// What the logs looked like when the newest scan began.
    revision: Option<Revision>,
    /// The line the search stands on.
    current: Option<Hit>,
    /// The scan finished or was replaced: scroll to the hit it finds.
    jump_pending: bool,
    /// A jump to this line is not finished: its first match must still be
    /// scrolled into view (the line may be taller than the log).
    reveal: Option<Reveal>,
    /// The last jump brought its match into view (rather than giving up).
    reveal_shown: bool,
    /// Where the line being revealed painted its first match, written during
    /// paint and read by the next frame.
    probe: Rc<Cell<Option<Probe>>>,
    /// The slice driver is running.
    running: bool,
    task: Option<Task<()>>,
    _input_changed: Subscription,
}

type Revision = ((u64, u64, usize), Option<Selection>);

/// A jump whose match may still be out of view, and how many frames were
/// spent bringing it in.
#[derive(Clone, Copy)]
struct Reveal {
    sequence: u64,
    tries: u8,
}

/// Where a line painted its first match: the line's sequence, the top of the
/// match in window coordinates and the height of a text line.
pub(crate) type Probe = (u64, Pixels, Pixels);

/// Frames a reveal may take before it gives up (each scrolls closer).
const REVEAL_TRIES: u8 = 8;

impl LogSearch {
    /// The matcher to draw highlights with and the line to emphasize.
    pub fn highlight(&self) -> Option<(&Query, Option<u64>)> {
        if !self.open {
            return None;
        }
        self.query
            .as_ref()
            .map(|query| (query, self.current.map(|hit| hit.sequence)))
    }
}

#[cfg(test)]
impl LogSearch {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn term(&self) -> &str {
        &self.term
    }

    pub(crate) fn hits(&self) -> &[Hit] {
        self.scan.as_ref().map_or(&[][..], |scan| scan.hits())
    }

    /// The last jump saw its match painted inside the log.
    pub(crate) fn revealed(&self) -> bool {
        self.reveal_shown
    }

    pub(crate) fn current(&self) -> Option<Hit> {
        self.current
    }

    pub(crate) fn input(&self) -> &Entity<TextInput> {
        &self.input
    }
}

impl ChatWindow {
    /// Opens the bar and focuses its field; with the bar open already, only
    /// focuses the field and selects its text.
    pub(crate) fn open_search(
        &mut self,
        _: &FindInLog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.server_menu = None;
        self.member_menu = None;
        self.channel_menu = None;
        self.log_menu = None;
        if self.search.is_none() {
            let placeholder = self.i18n.text("search_placeholder");
            let input = cx.new(|cx| TextInput::new_field(&placeholder, "", false, cx));
            let subscription = cx.observe_in(&input, window, |this, _, window, cx| {
                this.search_field_changed(window, cx)
            });
            self.search = Some(LogSearch {
                input,
                open: false,
                scope: Scope::Selected,
                term: String::new(),
                query: None,
                scan: None,
                pending: None,
                revision: None,
                current: None,
                jump_pending: false,
                reveal: None,
                reveal_shown: false,
                probe: Rc::default(),
                running: false,
                task: None,
                _input_changed: subscription,
            });
        }
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let was_open = search.open;
        search.open = true;
        let input = search.input.clone();
        let placeholder = self.i18n.text("search_placeholder");
        input.update(cx, |input, cx| {
            input.set_placeholder(&placeholder, cx);
            input.select_everything(cx);
        });
        window.focus(&input.focus_handle(cx));
        if !was_open {
            // A term kept from last time is searched again, in the logs as
            // they are now.
            self.search_begin(false, window, cx);
        }
        cx.notify();
    }

    /// The toolbar's magnifier: closes the bar when it is open, opens it
    /// otherwise.
    pub(crate) fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.as_ref().is_some_and(|search| search.open) {
            self.close_search(&CloseSearch, window, cx);
        } else {
            self.open_search(&FindInLog, window, cx);
        }
    }

    /// Closes the bar and returns to the draft. The line found stays where
    /// it is in the log.
    pub(crate) fn close_search(
        &mut self,
        _: &CloseSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.search.as_mut().filter(|search| search.open) else {
            return;
        };
        search.open = false;
        search.scan = None;
        search.pending = None;
        search.revision = None;
        search.task = None;
        search.running = false;
        search.jump_pending = false;
        search.reveal = None;
        if let Some(input) = self.inputs.get(&self.state.selection()) {
            window.focus(&input.focus_handle(cx));
        }
        cx.notify();
    }

    /// The matches of the open search in a line's `text`, and whether the
    /// line is the one the search stands on. Empty without a search, so a
    /// log not being searched pays nothing.
    pub(crate) fn search_ranges(
        &self,
        text: &str,
        sequence: u64,
    ) -> (Vec<std::ops::Range<usize>>, bool) {
        match self.search.as_ref().and_then(LogSearch::highlight) {
            Some((query, current)) => (query.ranges(text), current == Some(sequence)),
            None => (Vec::new(), false),
        }
    }

    /// Called when the field changed: a new term starts a new search, unless
    /// the input method is still composing it.
    fn search_field_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let (term, composing) = {
            let input = search.input.read(cx);
            (input.text().to_owned(), input.is_composing())
        };
        if composing || term == search.term {
            return;
        }
        search.query = Query::new(&term);
        search.term = term;
        if search.open {
            self.search_begin(true, window, cx);
        }
    }

    pub(crate) fn search_revision(&self, scope: Scope) -> Revision {
        (
            search::revision(&self.state, scope),
            (scope == Scope::Selected).then(|| self.state.selection()),
        )
    }

    /// Starts a scan for the committed term, replacing any running one.
    /// `jump`: scroll to a hit when it is found (the user typed) rather than
    /// only counting (the logs changed).
    fn search_begin(&mut self, jump: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        search.task = None;
        search.running = false;
        search.pending = None;
        let Some(query) = search.query.clone() else {
            search.scan = None;
            search.revision = None;
            search.current = None;
            cx.notify();
            return;
        };
        let scope = search.scope;
        let scan = Scan::new(&self.state, scope, query);
        let revision = self.search_revision(scope);
        let Some(search) = self.search.as_mut() else {
            return;
        };
        search.scan = Some(scan);
        search.revision = Some(revision);
        search.jump_pending |= jump;
        self.search_drive(DEBOUNCE, window, cx);
    }

    /// Runs the open scans in slices from the executor, after `delay`, until
    /// the logs are read and no longer ahead of the count.
    fn search_drive(&mut self, delay: Duration, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        search.running = true;
        search.task = Some(cx.spawn_in(window, async move |this, cx| {
            let mut delay = delay;
            loop {
                cx.background_executor().timer(delay).await;
                delay = RESCAN;
                loop {
                    let Ok(done) = this.update_in(cx, |this, _, cx| this.search_step(cx)) else {
                        return;
                    };
                    if done {
                        break;
                    }
                    yield_now().await;
                }
                let Ok(again) =
                    this.update_in(cx, |this, window, cx| this.search_finish(window, cx))
                else {
                    return;
                };
                if !again {
                    return;
                }
            }
        }));
        cx.notify();
    }

    /// Reads one slice of the newest scan; returns whether it is done. A
    /// finished re-read replaces the scan shown.
    fn search_step(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(search) = self.search.as_mut() else {
            return true;
        };
        let done = match (search.pending.as_mut(), search.scan.as_mut()) {
            (Some(pending), _) => {
                let done = pending.step(&self.state, SLICE);
                if done {
                    search.scan = search.pending.take();
                }
                done
            }
            (None, Some(scan)) => scan.step(&self.state, SLICE),
            (None, None) => true,
        };
        cx.notify();
        done
    }

    /// The scan shown is complete: settle on a hit (and go to it when the
    /// user asked for a search), then, when the logs have changed since the
    /// scan began, start a re-read in the background. Returns whether the
    /// driver goes on with it. The hits are published first, so a log that
    /// keeps changing cannot hold the result back.
    fn search_finish(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(search) = self.search.as_mut() else {
            return false;
        };
        let hits = search.scan.as_ref().map_or(&[][..], |scan| scan.hits());
        let still_there = search
            .current
            .is_some_and(|current| search::position(hits, current).is_some());
        let jump = std::mem::take(&mut search.jump_pending);
        if jump || !still_there {
            let chosen = search::initial_hit(hits, search.current);
            search.current = chosen;
            if let (true, Some(hit)) = (jump, chosen) {
                self.search_goto(hit, window, cx);
            }
        }
        cx.notify();
        self.search_start_reread()
    }

    /// Begins a re-read of the logs when they changed since the newest scan
    /// began. Returns whether it did.
    fn search_start_reread(&mut self) -> bool {
        let Some(search) = self.search.as_ref() else {
            return false;
        };
        let scope = search.scope;
        let revision = self.search_revision(scope);
        let Some(search) = self.search.as_mut() else {
            return false;
        };
        let Some(query) = search.query.clone() else {
            search.running = false;
            return false;
        };
        if search.revision == Some(revision) {
            search.running = false;
            return false;
        }
        search.revision = Some(revision);
        search.pending = Some(Scan::new(&self.state, scope, query));
        true
    }

    /// Re-reads a finished search whose logs have changed since, so the
    /// count follows new lines. Called while drawing; cheap when nothing
    /// changed.
    pub(crate) fn search_refresh_if_stale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        if !search.open || search.query.is_none() || search.running {
            return;
        }
        if self.search_start_reread() {
            self.search_drive(RESCAN, window, cx);
        }
    }

    /// Moves to the next or previous hit, wrapping at the ends.
    pub(crate) fn search_move(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let Some(scan) = search.scan.as_ref() else {
            return;
        };
        let hits = scan.hits();
        let mut at = search.current;
        let mut found = None;
        // Lines the log has dropped since the scan are skipped.
        for _ in 0..hits.len().min(64) + 1 {
            let next = if forward {
                search::next_hit(hits, at)
            } else {
                search::previous_hit(hits, at)
            };
            let Some(next) = next else {
                break;
            };
            at = Some(next);
            if search::hit_message(&self.state, next).is_some() {
                found = Some(next);
                break;
            }
        }
        let Some(hit) = found else {
            return;
        };
        if let Some(search) = self.search.as_mut() {
            search.current = Some(hit);
        }
        self.search_goto(hit, window, cx);
    }

    fn search_next(&mut self, _: &SearchNext, window: &mut Window, cx: &mut Context<Self>) {
        self.search_move(true, window, cx);
    }

    fn search_previous(&mut self, _: &SearchPrevious, window: &mut Window, cx: &mut Context<Self>) {
        self.search_move(false, window, cx);
    }

    /// Shows `hit`: selects its conversation or server log when that is not
    /// the selected one, and scrolls to the line. Focus stays where it is.
    fn search_goto(&mut self, hit: Hit, window: &mut Window, cx: &mut Context<Self>) {
        let (selection, command) = match hit.source {
            Source::Conversation(id) => (Selection::Channel(id), Command::SelectChannel(id)),
            Source::Server(network) => (Selection::Server(network), Command::SelectServer(network)),
        };
        if self.state.selection() != selection {
            self.dispatch_keeping_focus(command, window, cx);
        }
        self.sync_log_lists();
        if let Some(list) = self.main_lists.get_mut(&selection) {
            // The hit's own row goes to the top: rows above it may be taller
            // than the log and would push the match out of view. If the row
            // itself is taller than the log, `search_reveal_step` moves on to
            // where the row painted the match.
            list.scroll_to_sequence(hit.sequence);
            if let Some(search) = self.search.as_mut() {
                search.probe.set(None);
                search.reveal_shown = false;
                search.reveal = Some(Reveal {
                    sequence: hit.sequence,
                    tries: 0,
                });
            }
        }
        cx.notify();
    }

    /// Where the line `sequence` is to report its first match when painted,
    /// while a jump to it is being finished.
    pub(crate) fn search_probe(&self, sequence: u64) -> Option<Rc<Cell<Option<Probe>>>> {
        let search = self.search.as_ref()?;
        search
            .reveal
            .is_some_and(|reveal| reveal.sequence == sequence)
            .then(|| search.probe.clone())
    }

    /// The zero-size canvas a line adds while a jump to it is unfinished: when
    /// painted it reports where the byte `index` of the line's text sits in
    /// `layout`. Shared by every kind of log line (channel, DM, server log).
    pub(crate) fn search_probe_canvas(
        &self,
        sequence: u64,
        layout: TextLayout,
        index: Option<usize>,
    ) -> Option<Canvas<()>> {
        let (probe, index) = self.search_probe(sequence).zip(index)?;
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, _, _| {
                    if let Some(position) = layout.position_for_index(index) {
                        probe.set(Some((sequence, position.y, layout.line_height())));
                    }
                },
            )
            .absolute()
            .size_0(),
        )
    }

    /// Finishes a jump: reads where the line painted its first match in the
    /// last frame (from the real wrapped layout) and scrolls until that is
    /// inside the log. Asks for another frame while the match is not in view
    /// or the line has not been painted yet, a few times at most.
    pub(crate) fn search_reveal_step(&mut self, window: &mut Window) {
        let selection = self.state.selection();
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let Some(mut reveal) = search.reveal else {
            return;
        };
        let Some(list) = self.main_lists.get(&selection) else {
            search.reveal = None;
            return;
        };
        let viewport = list.state.viewport_bounds();
        if let Some((sequence, y, line)) = search.probe.take()
            && sequence == reveal.sequence
            && viewport.size.height > px(0.)
        {
            let (top, bottom) = (viewport.origin.y, viewport.origin.y + viewport.size.height);
            if y >= top && y + line <= bottom {
                search.reveal = None;
                search.reveal_shown = true;
                return;
            }
            // A third of the way down leaves a little of what precedes it.
            list.state.scroll_by(y - (top + viewport.size.height / 3.));
        }
        reveal.tries += 1;
        search.reveal = (reveal.tries < REVEAL_TRIES).then_some(reveal);
        if search.reveal.is_some() {
            window.request_animation_frame();
        }
    }

    /// Empties the field and puts the cursor back in it.
    pub(crate) fn search_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let input = search.input.clone();
        input.update(cx, |input, cx| input.set_text("", cx));
        window.focus(&input.focus_handle(cx));
    }

    pub(crate) fn search_set_scope(
        &mut self,
        scope: Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        if search.scope != scope {
            search.scope = scope;
            self.search_begin(true, window, cx);
        }
        if let Some(search) = self.search.as_ref() {
            window.focus(&search.input.focus_handle(cx));
        }
    }

    /// "3/12", "0 matches", or "…" while the logs are being read.
    fn search_status(&self) -> String {
        let Some(search) = self.search.as_ref() else {
            return String::new();
        };
        let (Some(_), Some(scan)) = (&search.query, &search.scan) else {
            return String::new();
        };
        let hits = scan.hits();
        if !scan.is_done() {
            return "…".to_owned();
        }
        if hits.is_empty() {
            return self.i18n.text("search_none");
        }
        let position = search
            .current
            .and_then(|current| search::position(hits, current))
            .map_or("–".to_owned(), |position| position.to_string());
        let more = if scan.is_truncated() { "+" } else { "" };
        format!("{position}/{}{more}", hits.len())
    }

    /// The bar, or nothing while it is closed.
    pub(crate) fn render_search_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let search = self.search.as_ref().filter(|search| search.open)?;
        let theme = crate::theme::current(cx);
        let scope = search.scope;
        let input = search.input.clone();
        let has_term = search.query.is_some();
        let has_hits = search
            .scan
            .as_ref()
            .is_some_and(|scan| !scan.hits().is_empty());
        let status = self.search_status();
        let on_server = matches!(self.state.selection(), Selection::Server(_));
        let selected_label = self.i18n.text(if on_server {
            "search_scope_log"
        } else {
            "search_scope_channel"
        });
        let all_label = self.i18n.text("search_scope_all");
        let scope_button =
            |id: &'static str, label: String, mine: Scope, cx: &mut Context<Self>| {
                let active = scope == mine;
                div()
                    .id(id)
                    .px(px(8.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(active, |d| d.bg(theme.selected))
                    .when(!active, |d| {
                        d.text_color(theme.text_secondary)
                            .hover(|style| style.bg(theme.hover_strong))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.search_set_scope(mine, window, cx)
                    }))
            };
        let bar = div()
            .id("log-search")
            .key_context("LogSearch")
            .flex()
            .items_center()
            .gap(px(4.))
            .w_full()
            .h(px(BAR_HEIGHT))
            .flex_shrink_0()
            .px(px(4.))
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .text_color(theme.text)
            // The field is an ordinary text input inside the chat window: its
            // Enter, arrow and Tab keys would reach the draft, so the bar
            // takes them.
            .on_action(
                cx.listener(|this, _: &SendMessage, window, cx| this.search_move(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Notice, window, cx| this.search_move(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &HistoryNext, window, cx| this.search_move(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &HistoryPrevious, window, cx| {
                this.search_move(false, window, cx)
            }))
            .on_action(cx.listener(|_, _: &CompleteNickname, _, _| {}))
            .on_action(cx.listener(Self::search_next))
            .on_action(cx.listener(Self::search_previous))
            .on_action(cx.listener(Self::close_search))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(
                svg()
                    .path("icons/ui/search.svg")
                    .size(px(16.))
                    .flex_shrink_0()
                    .ml(px(4.))
                    .text_color(theme.text_secondary),
            )
            .child(div().flex_1().min_w(px(80.)).child(input))
            .child(scope_button(
                "search-scope-selected",
                selected_label,
                Scope::Selected,
                cx,
            ))
            .child(scope_button("search-scope-all", all_label, Scope::All, cx))
            .child(
                div()
                    .min_w(px(48.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .text_color(if has_term && status == self.i18n.text("search_none") {
                        theme.warning
                    } else {
                        theme.text_secondary
                    })
                    .child(status),
            )
            .child(search_icon_button(
                &theme,
                "search-previous",
                "chevron-up",
                format!("{} (Shift+Enter)", self.i18n.text("search_previous")),
                has_hits,
                |this, window, cx| this.search_move(false, window, cx),
                cx,
            ))
            .child(search_icon_button(
                &theme,
                "search-next",
                "chevron-down",
                format!("{} (Enter)", self.i18n.text("search_next")),
                has_hits,
                |this, window, cx| this.search_move(true, window, cx),
                cx,
            ))
            .child(search_icon_button(
                &theme,
                "search-clear",
                "circle-x",
                self.i18n.text("search_clear"),
                has_term,
                |this, window, cx| this.search_clear(window, cx),
                cx,
            ))
            .child(search_icon_button(
                &theme,
                "search-close",
                "x",
                format!("{} (Esc)", self.i18n.text("search_close")),
                true,
                |this, window, cx| this.close_search(&CloseSearch, window, cx),
                cx,
            ));
        // The bar slides down from the top edge of the window content: its box grows
        // over a moment while the bar itself keeps its size, so the log below
        // is pushed down rather than covered, and keeps its scroll position.
        Some(
            div()
                .id("log-search-slide")
                .w_full()
                .flex_shrink_0()
                .overflow_hidden()
                .child(bar)
                .with_animation(
                    "log-search-slide-in",
                    Animation::new(SLIDE).with_easing(ease_out_quint()),
                    |slide, delta| slide.h(px(BAR_HEIGHT * delta)),
                )
                .into_any_element(),
        )
    }
}

/// A square icon button in the bar.
fn search_icon_button(
    theme: &Theme,
    id: &'static str,
    icon: &'static str,
    label: String,
    enabled: bool,
    on_click: fn(&mut ChatWindow, &mut Window, &mut Context<ChatWindow>),
    cx: &mut Context<ChatWindow>,
) -> impl IntoElement {
    let hover = theme.hover_strong;
    div()
        .id(id)
        .debug_selector(|| id.to_owned())
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(26.))
        .rounded(px(4.))
        .child(
            svg()
                .path(format!("icons/ui/{icon}.svg"))
                .size(px(16.))
                .text_color(if enabled {
                    theme.text
                } else {
                    theme.text_muted
                }),
        )
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
        })
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(label.clone().into())).into())
}
