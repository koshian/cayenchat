mod input;
mod localization;

use cayenchat_app::{AppState, Command, ConnectionStatus, Selection};
use cayenchat_irc_core::{Connection, ConnectionConfig, Event, SaslCredentials, WireDirection};
use cayenchat_model::{ConversationId, NetworkId};
use cayenchat_storage::{Appearance, Language, Settings, TextEncoding, color_value};
use gpui::{prelude::*, *};
use input::TextInput;
use localization::Localizer;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const DIAGNOSTIC_LIMIT: usize = 1000;
const SCROLL_BOTTOM_TOLERANCE: Pixels = px(2.);

struct LogScroll {
    handle: ScrollHandle,
    follow_latest: bool,
}

impl Default for LogScroll {
    fn default() -> Self {
        Self {
            handle: ScrollHandle::new(),
            follow_latest: true,
        }
    }
}

fn reaches_log_bottom(
    handle: &ScrollHandle,
    event: &ScrollWheelEvent,
    line_height: Pixels,
) -> bool {
    let max = handle.max_offset().height;
    let next = (handle.offset().y + event.delta.pixel_delta(line_height).y).clamp(-max, px(0.));
    next <= -max + SCROLL_BOTTOM_TOLERANCE
}

actions!(
    cayenchat,
    [
        CompleteNickname,
        SendMessage,
        Notice,
        OpenSettings,
        Disconnect,
        ToggleDebug,
        CopyDiagnostics,
        CopyLogSelection,
        Quit
    ]
);

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = cayenchat, no_json)]
struct Navigate {
    command: Command,
}

struct SettingsForm {
    values: Settings,
    server_list_open: bool,
    encoding_list_open: bool,
    custom_host: Entity<TextInput>,
    port: Entity<TextInput>,
    nickname: Entity<TextInput>,
    channels: Entity<TextInput>,
    server_password: Entity<TextInput>,
    sasl_username: Entity<TextInput>,
    sasl_password: Entity<TextInput>,
    member_list_background: Entity<TextInput>,
    main_log_background: Entity<TextInput>,
    main_log_alternate: Entity<TextInput>,
    sub_log_background: Entity<TextInput>,
    sub_log_alternate: Entity<TextInput>,
    main_log_font: Entity<TextInput>,
    sub_log_font: Entity<TextInput>,
    member_font: Entity<TextInput>,
    channel_font: Entity<TextInput>,
    input_font: Entity<TextInput>,
    time_font: Entity<TextInput>,
}

impl SettingsForm {
    fn new(values: Settings, i18n: &Localizer, cx: &mut Context<SettingsWindow>) -> Self {
        let field =
            |placeholder: &str, value: &str, secret: bool, cx: &mut Context<SettingsWindow>| {
                cx.new(|cx| TextInput::new_field(placeholder, value, secret, cx))
            };
        Self {
            custom_host: field(
                &i18n.text("server_host_placeholder"),
                &values.selected_profile().host,
                false,
                cx,
            ),
            port: field(
                "6667",
                &values.selected_profile().port.to_string(),
                false,
                cx,
            ),
            nickname: field(&i18n.text("nickname"), &values.nickname, false, cx),
            channels: field("#first,#second", &values.channels, false, cx),
            server_password: field(
                &i18n.text("server_password_placeholder"),
                values
                    .selected_profile()
                    .server_password
                    .as_deref()
                    .unwrap_or(""),
                true,
                cx,
            ),
            sasl_username: field(
                &i18n.text("sasl_account_placeholder"),
                &values.sasl_username,
                false,
                cx,
            ),
            sasl_password: field(
                &i18n.text("sasl_password"),
                values
                    .selected_profile()
                    .sasl_password
                    .as_deref()
                    .unwrap_or(""),
                true,
                cx,
            ),
            member_list_background: field(
                "#FFFFFF",
                &values.appearance.member_list_background,
                false,
                cx,
            ),
            main_log_background: field(
                "#FFFFFF",
                &values.appearance.main_log_background,
                false,
                cx,
            ),
            main_log_alternate: field("#F2F5FF", &values.appearance.main_log_alternate, false, cx),
            sub_log_background: field("#F9FAFB", &values.appearance.sub_log_background, false, cx),
            sub_log_alternate: field("#F2F5FF", &values.appearance.sub_log_alternate, false, cx),
            main_log_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.main_log_font,
                false,
                cx,
            ),
            sub_log_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.sub_log_font,
                false,
                cx,
            ),
            member_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.member_font,
                false,
                cx,
            ),
            channel_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.channel_font,
                false,
                cx,
            ),
            input_font: field(
                &i18n.text("font_system_placeholder"),
                &values.appearance.input_font,
                false,
                cx,
            ),
            time_font: field(
                &i18n.text("font_monospace_placeholder"),
                &values.appearance.time_font,
                false,
                cx,
            ),
            server_list_open: false,
            encoding_list_open: false,
            values,
        }
    }

    fn snapshot(&self, cx: &App) -> Result<Settings, String> {
        let mut settings = self.values.clone();
        let host = self.custom_host.read(cx).text().trim().to_owned();
        let port = self
            .port
            .read(cx)
            .text()
            .trim()
            .parse()
            .map_err(|_| i18n_error(settings.language, "port_invalid"))?;
        if port == 0 {
            return Err(i18n_error(settings.language, "port_invalid"));
        }
        let profile = settings.selected_profile_mut();
        if profile.custom {
            profile.host = host;
        }
        profile.port = port;
        if profile.remember_passwords {
            profile.server_password = Some(self.server_password.read(cx).text().to_owned());
            profile.sasl_password = Some(self.sasl_password.read(cx).text().to_owned());
        } else {
            profile.server_password = None;
            profile.sasl_password = None;
        }
        settings.nickname = self.nickname.read(cx).text().trim().to_owned();
        settings.channels = self.channels.read(cx).text().trim().to_owned();
        settings.sasl_username = self.sasl_username.read(cx).text().trim().to_owned();
        let value = |field: &Entity<TextInput>| field.read(cx).text().trim().to_owned();
        settings.appearance = Appearance {
            member_list_background: value(&self.member_list_background),
            main_log_background: value(&self.main_log_background),
            main_log_alternate: value(&self.main_log_alternate),
            sub_log_background: value(&self.sub_log_background),
            sub_log_alternate: value(&self.sub_log_alternate),
            alternate_rows: self.values.appearance.alternate_rows,
            main_log_font: value(&self.main_log_font),
            sub_log_font: value(&self.sub_log_font),
            member_font: value(&self.member_font),
            channel_font: value(&self.channel_font),
            input_font: value(&self.input_font),
            time_font: value(&self.time_font),
        };
        settings.appearance.validate()?;
        Ok(settings)
    }

    fn connection_config(&self, settings: &Settings, cx: &App) -> Result<ConnectionConfig, String> {
        let server_password = self.server_password.read(cx).text();
        let sasl_password = self.sasl_password.read(cx).text();
        connection_config(settings, server_password, sasl_password)
    }
}

fn connection_config(
    settings: &Settings,
    server_password: &str,
    sasl_password: &str,
) -> Result<ConnectionConfig, String> {
    let profile = settings.selected_profile();
    let mut config = ConnectionConfig::tls(
        profile.host.clone(),
        settings.nickname.clone(),
        settings.channels(),
    );
    config.port = profile.port;
    config.use_tls = profile.use_tls;
    config.verify_tls_certificates = profile.verify_tls_certificates;
    config.encoding = profile.encoding.label().into();
    if !server_password.is_empty() {
        config.server_password = Some(server_password.to_owned());
    }
    if settings.sasl_enabled {
        config.sasl = Some(SaslCredentials {
            username: settings.sasl_username.clone(),
            password: sasl_password.to_owned(),
        });
    }
    config.validate()?;
    Ok(config)
}

fn startup_connection_config(settings: &Settings) -> Option<Result<ConnectionConfig, String>> {
    settings.connect_on_startup.then(|| {
        let profile = settings.selected_profile();
        let (server_password, sasl_password) = if profile.remember_passwords {
            (
                profile.server_password.as_deref().unwrap_or(""),
                profile.sasl_password.as_deref().unwrap_or(""),
            )
        } else {
            ("", "")
        };
        connection_config(settings, server_password, sasl_password)
    })
}

fn i18n_error(language: Language, key: &str) -> String {
    Localizer::new(language).text(key)
}

struct ChatWindow {
    state: AppState,
    main_scroll: HashMap<Selection, LogScroll>,
    sub_scroll: LogScroll,
    // Editing and IME state belong to each server or channel.
    inputs: HashMap<Selection, Entity<TextInput>>,
    feedback: Option<String>,
    irc: Option<Connection>,
    startup_connection: Option<Result<ConnectionConfig, String>>,
    own_nickname: Option<String>,
    settings_window: Option<WindowHandle<SettingsWindow>>,
    diagnostics: Vec<String>,
    debug_enabled: bool,
    connection_started: Option<Instant>,
    watchdog_stage: u8,
    connection_generation: u64,
    appearance: Appearance,
    i18n: Localizer,
    log_focus: FocusHandle,
    log_selection: Option<LogSelection>,
    log_dragging: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LogPosition {
    row: usize,
    byte: usize,
}

#[derive(Clone, Copy)]
struct LogSelection {
    channel: ConversationId,
    anchor: LogPosition,
    cursor: LogPosition,
}

impl LogSelection {
    fn bounds(self) -> (LogPosition, LogPosition) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    fn range(self, row: usize, len: usize) -> Option<std::ops::Range<usize>> {
        let (start, end) = self.bounds();
        if row < start.row || row > end.row {
            return None;
        }
        let from = if row == start.row {
            start.byte.min(len)
        } else {
            0
        };
        let to = if row == end.row {
            end.byte.min(len)
        } else {
            len
        };
        (from < to).then_some(from..to)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Connection,
    Appearance,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FontTarget {
    MainLog,
    SubLog,
    Members,
    Channels,
    Input,
    Time,
}

struct SettingsWindow {
    owner: WindowHandle<ChatWindow>,
    settings: SettingsForm,
    feedback: Option<String>,
    tab: SettingsTab,
    font_picker: Option<FontTarget>,
    fonts: Vec<String>,
    i18n: Localizer,
}

impl ChatWindow {
    fn apply_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
        self.i18n = Localizer::new(language);
        let placeholder = self.i18n.text("draft_placeholder");
        for input in self.inputs.values() {
            input.update(cx, |input, cx| input.set_placeholder(&placeholder, cx));
        }
        cx.set_menus(app_menus(self.debug_enabled, &self.i18n));
        cx.notify();
        window.refresh();
    }

    fn status_text(&self, status: Option<&ConnectionStatus>) -> String {
        match status {
            Some(ConnectionStatus::OfflineMock) => self.i18n.text("status_offline"),
            Some(ConnectionStatus::Connecting) => self.i18n.text("status_connecting"),
            Some(ConnectionStatus::TransportConnected) => self.i18n.text("status_registering"),
            Some(ConnectionStatus::Registered) => self.i18n.text("status_connected"),
            Some(ConnectionStatus::Disconnected(reason)) => self
                .i18n
                .format("status_disconnected", &[("reason", reason)]),
            None => self.i18n.text("status_unknown"),
        }
    }

    fn update_title(&self, window: &mut Window) {
        let network = self.state.selected_network();
        let title = match self.state.selected_channel() {
            Some(channel) => format!("{} @ {} — CayenChat", channel.name, network.name),
            None => format!("{} — CayenChat", network.name),
        };
        window.set_window_title(&title);
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (saved, feedback) = match cayenchat_storage::load() {
            Ok(saved) => (saved.unwrap_or_default(), None),
            Err(error) => (Settings::default(), Some(error)),
        };
        let i18n = Localizer::new(saved.language);
        let startup_connection = startup_connection_config(&saved);
        let state = AppState::configured(saved.selected_profile().host.clone(), saved.channels());
        let mut inputs: HashMap<_, _> = state
            .networks()
            .iter()
            .map(|server| {
                let placeholder = i18n.text("draft_placeholder");
                (
                    Selection::Server(server.id),
                    cx.new(|cx| TextInput::new_live(&placeholder, cx)),
                )
            })
            .collect();
        inputs.extend(state.conversations().iter().map(|channel| {
            let placeholder = i18n.text("draft_placeholder");
            (
                Selection::Channel(channel.id),
                cx.new(|cx| TextInput::new_live(&placeholder, cx)),
            )
        }));
        window.focus(&inputs[&state.selection()].focus_handle(cx));
        let this = Self {
            state,
            main_scroll: HashMap::new(),
            sub_scroll: LogScroll::default(),
            inputs,
            feedback,
            irc: None,
            startup_connection,
            own_nickname: None,
            settings_window: None,
            diagnostics: Vec::new(),
            debug_enabled: false,
            connection_started: None,
            watchdog_stage: 0,
            connection_generation: 0,
            appearance: saved.appearance,
            i18n,
            log_focus: cx.focus_handle(),
            log_selection: None,
            log_dragging: false,
        };
        this.update_title(window);
        this
    }

    fn spawn_poll(&self, cx: &mut Context<Self>) {
        let generation = self.connection_generation;
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(50)).await;
                let keep_polling = match this.update(cx, |this, cx| {
                    if this.connection_generation == generation {
                        this.poll_events(cx)
                    } else {
                        false
                    }
                }) {
                    Ok(keep_polling) => keep_polling,
                    Err(_) => break,
                };
                if !keep_polling {
                    break;
                }
            }
        })
        .detach();
    }

    fn apply_connection(
        &mut self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if let Some(connection) = self.irc.take() {
            let _ = connection.disconnect();
        }
        self.connection_generation += 1;
        self.diagnostics.clear();
        self.connection_started = Some(Instant::now());
        self.watchdog_stage = 0;
        self.state = AppState::live(config.host.clone(), config.channels.clone());
        self.main_scroll.clear();
        self.sub_scroll = LogScroll::default();
        self.log_selection = None;
        let placeholder = self.i18n.text("draft_placeholder");
        self.inputs = self
            .state
            .networks()
            .iter()
            .map(|server| {
                (
                    Selection::Server(server.id),
                    cx.new(|cx| TextInput::new_live(&placeholder, cx)),
                )
            })
            .collect();
        self.inputs
            .extend(self.state.conversations().iter().map(|channel| {
                (
                    Selection::Channel(channel.id),
                    cx.new(|cx| TextInput::new_live(&placeholder, cx)),
                )
            }));
        self.own_nickname = Some(config.nickname.clone());
        let outcome = match Connection::connect(config) {
            Ok(connection) => {
                self.irc = Some(connection);
                self.feedback = None;
                self.spawn_poll(cx);
                window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
                Ok(())
            }
            Err(error) => {
                self.state
                    .set_status(NetworkId(1), ConnectionStatus::Disconnected(error.clone()));
                self.feedback = Some(error.clone());
                Err(error)
            }
        };
        self.update_title(window);
        cx.notify();
        window.refresh();
        outcome
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(connection) = &self.irc {
            self.feedback = connection.disconnect().err();
            cx.notify();
        }
    }

    fn disconnect_action(&mut self, _: &Disconnect, _: &mut Window, cx: &mut Context<Self>) {
        self.disconnect(cx);
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.settings_window
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        let settings = match cayenchat_storage::load() {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
                return;
            }
        };
        let owner = window.window_handle().downcast::<ChatWindow>().unwrap();
        let bounds = Bounds::centered(None, size(px(740.), px(750.)), cx);
        match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(620.), px(540.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(self.i18n.text("settings_title").into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| SettingsWindow::new(owner, settings, window, cx)),
        ) {
            Ok(handle) => self.settings_window = Some(handle),
            Err(error) => {
                self.feedback = Some(
                    self.i18n
                        .format("settings_open_failed", &[("error", &error.to_string())]),
                )
            }
        }
        cx.notify();
    }

    fn toggle_debug(&mut self, _: &ToggleDebug, _: &mut Window, cx: &mut Context<Self>) {
        self.debug_enabled = !self.debug_enabled;
        cx.set_menus(app_menus(self.debug_enabled, &self.i18n));
        cx.notify();
    }

    fn copy_diagnostics(&mut self, _: &CopyDiagnostics, _: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.diagnostics.join("\n")));
    }

    fn start_log_selection(
        &mut self,
        channel: ConversationId,
        row: usize,
        byte: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = LogPosition { row, byte };
        self.log_selection = Some(LogSelection {
            channel,
            anchor: position,
            cursor: position,
        });
        self.log_dragging = true;
        window.focus(&self.log_focus);
        cx.notify();
    }

    fn extend_log_selection(
        &mut self,
        channel: ConversationId,
        row: usize,
        byte: usize,
        cx: &mut Context<Self>,
    ) {
        if self.log_dragging
            && let Some(selection) = self.log_selection.as_mut()
            && selection.channel == channel
        {
            selection.cursor = LogPosition { row, byte };
            cx.notify();
        }
    }

    fn finish_log_selection(&mut self) {
        self.log_dragging = false;
    }

    fn copy_selected_log(&mut self, cx: &mut Context<Self>) {
        let (Some(selection), Some(channel)) = (self.log_selection, self.state.selected_channel())
        else {
            return;
        };
        if selection.channel != channel.id {
            return;
        }
        let (start, end) = selection.bounds();
        if start == end {
            return;
        }
        let pieces: Vec<_> = (start.row..=end.row)
            .filter_map(|row| {
                channel.messages.get(row).map(|message| {
                    let range = selection.range(row, message.text.len()).unwrap_or(0..0);
                    message.text[range].to_owned()
                })
            })
            .collect();
        if !pieces.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(pieces.join("\n")));
        }
    }

    fn copy_log_selection(&mut self, _: &CopyLogSelection, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected_log(cx);
    }

    fn copy_log_selection_menu(&mut self, _: &input::Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected_log(cx);
    }

    fn push_diagnostic(&mut self, line: String) {
        self.diagnostics.push(line);
        if self.diagnostics.len() > DIAGNOSTIC_LIMIT {
            self.diagnostics.remove(0);
        }
    }

    fn dispatch(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        self.state.dispatch(command);
        self.log_selection = None;
        self.feedback = None;
        self.update_title(window);
        window.focus(&self.inputs[&self.state.selection()].focus_handle(cx));
        cx.notify();
        // Redraw the pane contents together with the native title change.
        window.refresh();
    }

    fn navigate(&mut self, action: &Navigate, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(action.command, window, cx);
    }

    fn complete_nickname(
        &mut self,
        _: &CompleteNickname,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(channel) = self.state.selected_channel() else {
            return;
        };
        let members = channel.members.clone();
        let input = self.inputs[&self.state.selection()].clone();
        if input.read(cx).is_composing() {
            return;
        }
        input.update(cx, |input, cx| {
            input.complete_nickname(&members, window, cx)
        });
    }

    fn poll_events(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        let mut disconnected = false;
        for _ in 0..64 {
            let event = self.irc.as_mut().and_then(Connection::try_recv);
            let Some(event) = event else {
                break;
            };
            changed = true;
            if matches!(event, Event::Disconnected(_)) {
                disconnected = true;
            }
            self.handle_event(event);
        }
        if let Some(started) = self.connection_started
            && self.state.status(NetworkId(1)) == Some(&ConnectionStatus::Connecting)
        {
            let elapsed = started.elapsed();
            let stage = if elapsed >= Duration::from_secs(15) {
                2
            } else if elapsed >= Duration::from_secs(5) {
                1
            } else {
                0
            };
            if stage > self.watchdog_stage {
                self.watchdog_stage = stage;
                self.push_diagnostic(format!("[{:.1}s] UI is still waiting for the transport worker; inspect the latest diagnostic stage.", elapsed.as_secs_f32()));
                changed = true;
            }
        }
        if changed {
            let placeholder = self.i18n.text("draft_placeholder");
            for channel in self.state.conversations() {
                let key = Selection::Channel(channel.id);
                self.inputs
                    .entry(key)
                    .or_insert_with(|| cx.new(|cx| TextInput::new_live(&placeholder, cx)));
            }
            cx.notify();
        }
        let worker_closed = self.irc.as_ref().is_some_and(Connection::is_closed);
        if worker_closed && !disconnected {
            self.push_diagnostic("IRC worker ended without a disconnect event.".into());
            self.handle_event(Event::Disconnected(
                "IRC worker stopped unexpectedly.".into(),
            ));
            cx.notify();
        }
        if disconnected || worker_closed {
            self.irc = None;
            return false;
        }
        true
    }

    fn handle_event(&mut self, event: Event) {
        let network = NetworkId(1);
        match event {
            Event::Diagnostic { elapsed, message } => {
                self.push_diagnostic(format!("[{:.1}s] {message}", elapsed.as_secs_f32()));
            }
            Event::Wire {
                elapsed,
                direction,
                line,
            } => {
                let arrow = match direction {
                    WireDirection::Sent => "→",
                    WireDirection::Received => "←",
                };
                self.push_diagnostic(format!("[{:.1}s] {arrow} {line}", elapsed.as_secs_f32()));
            }
            Event::TransportConnected => {
                self.connection_started = None;
                self.state
                    .set_status(network, ConnectionStatus::TransportConnected);
                self.state
                    .append_server_message(network, self.i18n.text("event_transport_connected"));
            }
            Event::Registered { nickname } => {
                self.connection_started = None;
                self.own_nickname = Some(nickname.clone());
                self.state.set_status(network, ConnectionStatus::Registered);
                self.state.append_server_message(
                    network,
                    self.i18n
                        .format("event_registered", &[("nickname", &nickname)]),
                );
            }
            Event::Joined { channel } => {
                self.state.joined_channel(network, &channel);
                self.state.append_server_message(
                    network,
                    self.i18n.format("event_joined", &[("channel", &channel)]),
                );
            }
            Event::Parted { channel } => {
                self.state.parted_channel(network, &channel);
                self.state.append_server_message(
                    network,
                    self.i18n.format("event_parted", &[("channel", &channel)]),
                );
            }
            Event::NickChanged { nickname } => {
                self.own_nickname = Some(nickname.clone());
                self.state.append_server_message(
                    network,
                    self.i18n
                        .format("event_nick_changed", &[("nickname", &nickname)]),
                );
            }
            Event::ChannelMessage {
                channel,
                sender,
                text,
                notice,
            } => {
                self.state
                    .append_channel_message(network, &channel, &sender, &text, notice);
            }
            Event::Names { channel, users } => self.state.set_members(network, &channel, users),
            Event::ServerLine(line) => self.state.append_server_message(network, line),
            Event::OutgoingAccepted {
                channel,
                text,
                notice,
            } => {
                if channel.starts_with(['#', '&']) {
                    let nickname = self.own_nickname.as_deref().unwrap_or("me");
                    self.state
                        .append_channel_message(network, &channel, nickname, &text, notice);
                } else {
                    self.state.append_server_message(
                        network,
                        self.i18n
                            .format("event_message_queued", &[("channel", &channel)]),
                    );
                }
            }
            Event::Disconnected(reason) => {
                self.connection_started = None;
                self.push_diagnostic(format!("Disconnected: {reason}"));
                self.state
                    .set_status(network, ConnectionStatus::Disconnected(reason.clone()));
                let message = self
                    .i18n
                    .format("status_disconnected", &[("reason", &reason)]);
                self.state.append_server_message(network, message.clone());
                self.feedback = Some(message);
            }
        }
    }

    fn send_draft(&mut self, notice: bool, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.state.selection();
        let input = self.inputs[&selection].clone();
        if input.read(cx).is_composing() {
            return;
        }
        let text = input.read(cx).text().to_owned();
        if text.trim().is_empty() {
            return;
        }
        let selected = self.state.selected_channel();
        let result = if let Some(connection) = &self.irc {
            if self.state.status(NetworkId(1)) != Some(&ConnectionStatus::Registered) {
                Err(self.i18n.text("wait_registration"))
            } else if text.starts_with('/') {
                connection.send_command(&text, selected.map(|channel| channel.name.as_str()))
            } else if let Some(channel) = selected {
                if !self.state.is_active_channel(channel.id) {
                    Err(self.i18n.text("wait_join"))
                } else {
                    connection.send_message(&channel.name, &text, notice)
                }
            } else {
                Err(self.i18n.text("select_channel"))
            }
        } else {
            Err(self.i18n.text("not_connected"))
        };
        self.feedback = match result {
            Ok(()) => {
                input.update(cx, |input, cx| input.clear_after_send(cx));
                None
            }
            Err(error) => Some(error),
        };
        cx.notify();
        window.refresh();
    }

    fn send_message(&mut self, _: &SendMessage, window: &mut Window, cx: &mut Context<Self>) {
        self.send_draft(false, window, cx);
    }

    fn notice(&mut self, _: &Notice, window: &mut Window, cx: &mut Context<Self>) {
        self.send_draft(true, window, cx);
    }
}

impl SettingsWindow {
    fn select_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.values.language = language;
        self.i18n = Localizer::new(language);
        window.set_window_title(&self.i18n.text("settings_title"));
        for (field, key) in [
            (&self.settings.custom_host, "server_host_placeholder"),
            (&self.settings.nickname, "nickname"),
            (
                &self.settings.server_password,
                "server_password_placeholder",
            ),
            (&self.settings.sasl_username, "sasl_account_placeholder"),
            (&self.settings.sasl_password, "sasl_password"),
            (&self.settings.main_log_font, "font_system_placeholder"),
            (&self.settings.sub_log_font, "font_system_placeholder"),
            (&self.settings.member_font, "font_system_placeholder"),
            (&self.settings.channel_font, "font_system_placeholder"),
            (&self.settings.input_font, "font_system_placeholder"),
            (&self.settings.time_font, "font_monospace_placeholder"),
        ] {
            let placeholder = self.i18n.text(key);
            field.update(cx, |field, cx| field.set_placeholder(&placeholder, cx));
        }
        self.feedback = None;
        cx.notify();
    }

    fn new(
        owner: WindowHandle<ChatWindow>,
        values: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let i18n = Localizer::new(values.language);
        let settings = SettingsForm::new(values, &i18n, cx);
        window.focus(&settings.nickname.focus_handle(cx));
        let mut fonts = window.text_system().all_font_names();
        fonts.sort_unstable();
        fonts.dedup();
        Self {
            owner,
            settings,
            feedback: None,
            tab: SettingsTab::Connection,
            font_picker: None,
            fonts,
            i18n,
        }
    }

    fn connect_from_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = (|| {
            let mut settings = self.settings.snapshot(cx)?;
            let config = self.settings.connection_config(&settings, cx)?;
            settings
                .servers
                .retain(|server| !server.custom || !server.host.is_empty());
            cayenchat_storage::save(&settings)?;
            Ok::<_, String>((config, settings.appearance.clone(), settings.language))
        })();
        self.feedback = match result {
            Ok((config, appearance, language)) => {
                match self.owner.update(cx, |owner, chat_window, cx| {
                    owner.appearance = appearance;
                    owner.apply_language(language, chat_window, cx);
                    owner.apply_connection(config, chat_window, cx)
                }) {
                    Ok(Ok(())) => {
                        window.remove_window();
                        return;
                    }
                    Ok(Err(error)) => Some(error),
                    Err(error) => Some(
                        self.i18n
                            .format("chat_closed", &[("error", &error.to_string())]),
                    ),
                }
            }
            Err(error) => Some(error),
        };
        cx.notify();
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        self.feedback = match self.settings.snapshot(cx).and_then(|mut settings| {
            if settings.selected_profile().host.is_empty() {
                return Err(self.i18n.text("server_required"));
            }
            settings
                .servers
                .retain(|server| !server.custom || !server.host.is_empty());
            cayenchat_storage::save(&settings)?;
            self.settings.values = settings;
            let appearance = self.settings.values.appearance.clone();
            let language = self.settings.values.language;
            let _ = self.owner.update(cx, |owner, window, cx| {
                owner.appearance = appearance;
                owner.apply_language(language, window, cx);
            });
            Ok(())
        }) {
            Ok(()) => Some(self.i18n.text("settings_saved")),
            Err(error) => Some(error),
        };
        cx.notify();
    }

    fn close_settings(&mut self, window: &mut Window) {
        window.remove_window();
    }

    fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.activate_window();
    }

    fn disconnect_action(&mut self, _: &Disconnect, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.owner.update(cx, |owner, _, cx| owner.disconnect(cx));
        cx.notify();
    }

    fn toggle_debug_action(&mut self, _: &ToggleDebug, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.owner.update(cx, |owner, window, cx| {
            owner.toggle_debug(&ToggleDebug, window, cx)
        });
    }

    fn copy_diagnostics_action(
        &mut self,
        _: &CopyDiagnostics,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.owner.update(cx, |owner, window, cx| {
            owner.copy_diagnostics(&CopyDiagnostics, window, cx)
        });
    }

    fn toggle_remember_passwords(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.values.selected_profile().remember_passwords {
            let id = self.settings.values.selected_server.clone();
            match cayenchat_storage::clear_saved_passwords(&id) {
                Ok(()) => {
                    let profile = self.settings.values.selected_profile_mut();
                    profile.remember_passwords = false;
                    profile.server_password = None;
                    profile.sasl_password = None;
                    self.settings
                        .server_password
                        .update(cx, |field, cx| field.set_text("", cx));
                    self.settings
                        .sasl_password
                        .update(cx, |field, cx| field.set_text("", cx));
                    self.feedback = Some(self.i18n.text("passwords_removed"));
                }
                Err(error) => self.feedback = Some(error),
            }
            cx.notify();
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &self.i18n.text("password_warning_title"),
            Some(&self.i18n.text("password_warning_detail")),
            &[
                PromptButton::ok(self.i18n.text("save_passwords")),
                PromptButton::cancel(self.i18n.text("cancel")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    this.settings
                        .values
                        .selected_profile_mut()
                        .remember_passwords = true;
                    this.feedback = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn toggle_tls(&mut self, cx: &mut Context<Self>) {
        if self.settings.values.selected_profile().use_tls && self.settings.values.sasl_enabled {
            self.feedback = Some(self.i18n.text("disable_sasl_first"));
            cx.notify();
            return;
        }
        self.feedback = None;
        let use_tls = !self.settings.values.selected_profile().use_tls;
        let profile = self.settings.values.selected_profile_mut();
        profile.use_tls = use_tls;
        if !use_tls {
            profile.verify_tls_certificates = true;
        }
        let port = self.settings.port.read(cx).text().to_owned();
        if port == "6667" && use_tls {
            self.settings
                .port
                .update(cx, |field, cx| field.set_text("6697", cx));
        } else if port == "6697" && !use_tls {
            self.settings
                .port
                .update(cx, |field, cx| field.set_text("6667", cx));
        }
        cx.notify();
    }

    fn toggle_certificate_verification(&mut self, cx: &mut Context<Self>) {
        let profile = self.settings.values.selected_profile_mut();
        if profile.use_tls {
            profile.verify_tls_certificates = !profile.verify_tls_certificates;
            cx.notify();
        }
    }

    fn toggle_sasl(&mut self, cx: &mut Context<Self>) {
        self.feedback = None;
        self.settings.values.sasl_enabled = !self.settings.values.sasl_enabled;
        if self.settings.values.sasl_enabled && !self.settings.values.selected_profile().use_tls {
            self.toggle_tls(cx);
        }
        cx.notify();
    }

    fn show_selected_server(&mut self, cx: &mut Context<Self>) {
        let profile = self.settings.values.selected_profile().clone();
        self.settings
            .custom_host
            .update(cx, |field, cx| field.set_text(&profile.host, cx));
        self.settings.port.update(cx, |field, cx| {
            field.set_text(&profile.port.to_string(), cx)
        });
        self.settings.server_password.update(cx, |field, cx| {
            field.set_text(profile.server_password.as_deref().unwrap_or(""), cx)
        });
        self.settings.sasl_password.update(cx, |field, cx| {
            field.set_text(profile.sasl_password.as_deref().unwrap_or(""), cx)
        });
        self.settings.server_list_open = false;
        self.settings.encoding_list_open = false;
        self.feedback = None;
        cx.notify();
    }

    fn select_server(&mut self, id: String, cx: &mut Context<Self>) {
        match self.settings.snapshot(cx) {
            Ok(mut settings) => {
                settings.selected_server = id;
                self.settings.values = settings;
                self.show_selected_server(cx);
            }
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
            }
        }
    }

    fn add_server(&mut self, cx: &mut Context<Self>) {
        match self.settings.snapshot(cx) {
            Ok(mut settings) => {
                settings.add_custom_server();
                self.settings.values = settings;
                self.show_selected_server(cx);
            }
            Err(error) => {
                self.feedback = Some(error);
                cx.notify();
            }
        }
    }

    fn remove_server(&mut self, cx: &mut Context<Self>) {
        self.settings.values.remove_selected_custom_server();
        self.show_selected_server(cx);
    }

    fn render_connection_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let profile = self.settings.values.selected_profile().clone();
        let tls = profile.use_tls;
        let sasl = self.settings.values.sasl_enabled;
        let mut language_selector = div().flex().gap_1();
        for (index, language) in [Language::System, Language::Japanese, Language::English]
            .into_iter()
            .enumerate()
        {
            language_selector = language_selector.child(
                div()
                    .id(("language-option", index))
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(rgb(0xb7bdc4))
                    .cursor_pointer()
                    .when(self.settings.values.language == language, |d| {
                        d.bg(rgb(0xcbdbea))
                    })
                    .child(self.i18n.preference_label(language))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_language(language, window, cx)
                    })),
            );
        }
        let current_host = if profile.custom {
            self.settings.custom_host.read(cx).text().trim().to_owned()
        } else {
            profile.host.clone()
        };
        let current_port = self.settings.port.read(cx).text().trim().to_owned();
        let selected_label = if current_host.is_empty() {
            self.i18n.text("new_server")
        } else {
            format!("{current_host}:{current_port}")
        };
        let mut server_selector = div().flex().flex_col().child(
            div()
                .id("server-select")
                .px_2()
                .py_1()
                .border_1()
                .border_color(rgb(0xb7bdc4))
                .cursor_pointer()
                .child(format!("{selected_label}  ▾"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings.server_list_open = !this.settings.server_list_open;
                    this.settings.encoding_list_open = false;
                    cx.notify();
                })),
        );
        if self.settings.server_list_open {
            let mut menu = div()
                .border_1()
                .border_color(rgb(0xb7bdc4))
                .bg(rgb(0xffffff));
            for (index, server) in self.settings.values.ordered_servers().enumerate() {
                let id = server.id.clone();
                let (host, port) = if id == profile.id {
                    (current_host.as_str(), current_port.as_str())
                } else {
                    (server.host.as_str(), "")
                };
                let label = if host.is_empty() {
                    self.i18n.text("new_server")
                } else {
                    let kind = self.i18n.text(if server.custom {
                        "custom_server"
                    } else {
                        "preset_server"
                    });
                    format!(
                        "{}:{}{}",
                        host,
                        if port.is_empty() {
                            server.port.to_string()
                        } else {
                            port.to_owned()
                        },
                        kind
                    )
                };
                menu = menu.child(
                    div()
                        .id(("server-option", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(0xe8eff6)))
                        .when(profile.id == id, |d| d.bg(rgb(0xcbdbea)))
                        .child(label)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select_server(id.clone(), cx)),
                        ),
                );
            }
            menu = menu.child(
                div()
                    .id("add-server-option")
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(rgb(0xb7bdc4))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(0xe8eff6)))
                    .child(self.i18n.text("add_server"))
                    .on_click(cx.listener(|this, _, _, cx| this.add_server(cx))),
            );
            server_selector = server_selector.child(menu);
        }
        let mut encoding_selector = div().flex().flex_col().child(
            div()
                .id("encoding-select")
                .px_2()
                .py_1()
                .border_1()
                .border_color(rgb(0xb7bdc4))
                .cursor_pointer()
                .child(format!("{}  ▾", profile.encoding.label()))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings.encoding_list_open = !this.settings.encoding_list_open;
                    this.settings.server_list_open = false;
                    cx.notify();
                })),
        );
        if self.settings.encoding_list_open {
            let mut menu = div()
                .border_1()
                .border_color(rgb(0xb7bdc4))
                .bg(rgb(0xffffff));
            for (index, encoding) in TextEncoding::ALL.into_iter().enumerate() {
                menu = menu.child(
                    div()
                        .id(("encoding-option", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(0xe8eff6)))
                        .when(profile.encoding == encoding, |d| d.bg(rgb(0xcbdbea)))
                        .child(encoding.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.values.selected_profile_mut().encoding = encoding;
                            this.settings.encoding_list_open = false;
                            cx.notify();
                        })),
                );
            }
            encoding_selector = encoding_selector.child(menu);
        }
        div()
            .w(px(680.))
            .p_4()
            .my_4()
            .bg(rgb(0xffffff))
            .border_1()
            .border_color(rgb(0xb7bdc4))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("connection")),
            )
            .child(
                div()
                    .text_color(rgb(0x52606c))
                    .child(self.i18n.text("connection_intro")),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .child(self.i18n.text("language")),
                    )
                    .child(language_selector),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(rgb(0x52606c))
                    .child(self.i18n.text("language_hint")),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .child(self.i18n.text("destination")),
                    )
                    .child(server_selector.flex_1().min_w_0()),
            )
            .when(profile.custom, |d| {
                d.child(settings_field(
                    &self.i18n.text("host"),
                    self.settings.custom_host.clone(),
                ))
                .child(
                    div()
                        .id("remove-server")
                        .ml(px(158.))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .text_color(rgb(0x9a4b28))
                        .child(self.i18n.text("remove_server"))
                        .on_click(cx.listener(|this, _, _, cx| this.remove_server(cx))),
                )
            })
            .child(settings_field(
                &self.i18n.text("port"),
                self.settings.port.clone(),
            ))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .child(self.i18n.text("encoding")),
                    )
                    .child(encoding_selector.flex_1().min_w_0()),
            )
            .child(
                div()
                    .ml(px(158.))
                    .text_color(rgb(0x52606c))
                    .child(self.i18n.text("encoding_hint")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(150.)).child("TLS/SSL"))
                    .child(
                        div()
                            .id("tls-toggle")
                            .px_2()
                            .py_1()
                            .border_1()
                            .border_color(rgb(0xb7bdc4))
                            .cursor_pointer()
                            .child(self.i18n.text(if tls { "on" } else { "off" }))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_tls(cx))),
                    ),
            )
            .when(tls, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(150.))
                                .child(self.i18n.text("verify_certificates")),
                        )
                        .child(
                            div()
                                .id("certificate-verification-toggle")
                                .px_2()
                                .py_1()
                                .border_1()
                                .border_color(rgb(0xb7bdc4))
                                .cursor_pointer()
                                .child(self.i18n.text(if profile.verify_tls_certificates {
                                    "on"
                                } else {
                                    "off"
                                }))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_certificate_verification(cx)
                                })),
                        ),
                )
                .when(!profile.verify_tls_certificates, |d| {
                    d.child(
                        div()
                            .ml(px(158.))
                            .text_color(rgb(0x9a4b28))
                            .child(self.i18n.text("certificate_warning")),
                    )
                })
            })
            .child(settings_field(
                &self.i18n.text("nickname"),
                self.settings.nickname.clone(),
            ))
            .child(settings_field(
                &self.i18n.text("auto_join_channels"),
                self.settings.channels.clone(),
            ))
            .child(
                div()
                    .id("connect-on-startup")
                    .ml(px(158.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(if self.settings.values.connect_on_startup {
                        "☑"
                    } else {
                        "☐"
                    })
                    .child(self.i18n.text("connect_on_startup"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.values.connect_on_startup =
                            !this.settings.values.connect_on_startup;
                        cx.notify();
                    })),
            )
            .child(settings_field(
                &self.i18n.text("server_password"),
                self.settings.server_password.clone(),
            ))
            .child(
                div()
                    .id("remember-passwords")
                    .ml(px(158.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(if profile.remember_passwords {
                        "☑"
                    } else {
                        "☐"
                    })
                    .child(self.i18n.text("remember_passwords"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_remember_passwords(window, cx)
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(150.)).child("SASL PLAIN"))
                    .child(
                        div()
                            .id("sasl-toggle")
                            .px_2()
                            .py_1()
                            .border_1()
                            .border_color(rgb(0xb7bdc4))
                            .cursor_pointer()
                            .child(self.i18n.text(if sasl { "on" } else { "off" }))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sasl(cx))),
                    ),
            )
            .when(sasl, |d| {
                d.child(settings_field(
                    &self.i18n.text("sasl_account"),
                    self.settings.sasl_username.clone(),
                ))
                .child(settings_field(
                    &self.i18n.text("sasl_password"),
                    self.settings.sasl_password.clone(),
                ))
            })
            .when_some(self.feedback.clone(), |d, feedback| {
                d.child(div().text_color(rgb(0x9a4b28)).child(feedback))
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        div()
                            .id("connect-button")
                            .px_3()
                            .py_1()
                            .bg(rgb(0xcbdbea))
                            .cursor_pointer()
                            .child(self.i18n.text("save_and_connect"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.connect_from_settings(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .id("save-button")
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(rgb(0xb7bdc4))
                            .cursor_pointer()
                            .child(self.i18n.text("save"))
                            .on_click(cx.listener(|this, _, _, cx| this.save_settings(cx))),
                    )
                    .child(
                        div()
                            .id("back-button")
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(rgb(0xb7bdc4))
                            .cursor_pointer()
                            .child(self.i18n.text("back"))
                            .on_click(
                                cx.listener(|this, _, window, _| this.close_settings(window)),
                            ),
                    )
                    .child(
                        div()
                            .id("disconnect-button")
                            .px_3()
                            .py_1()
                            .border_1()
                            .border_color(rgb(0xb7bdc4))
                            .cursor_pointer()
                            .child(self.i18n.text("disconnect"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let _ = this.owner.update(cx, |owner, _, cx| owner.disconnect(cx));
                                cx.notify();
                            })),
                    ),
            )
    }

    fn font_input(&self, target: FontTarget) -> Entity<TextInput> {
        match target {
            FontTarget::MainLog => self.settings.main_log_font.clone(),
            FontTarget::SubLog => self.settings.sub_log_font.clone(),
            FontTarget::Members => self.settings.member_font.clone(),
            FontTarget::Channels => self.settings.channel_font.clone(),
            FontTarget::Input => self.settings.input_font.clone(),
            FontTarget::Time => self.settings.time_font.clone(),
        }
    }

    fn font_field(&self, target: FontTarget, label: &str, cx: &mut Context<Self>) -> Div {
        let input = self.font_input(target);
        let mut field = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(150.)).flex_shrink_0().child(label.to_owned()))
                .child(div().flex_1().min_w_0().child(input.clone()))
                .child(
                    div()
                        .id(("font-picker", target as u32))
                        .px_2()
                        .py_1()
                        .border_1()
                        .border_color(rgb(0xb7bdc4))
                        .cursor_pointer()
                        .child(self.i18n.text("choose_font"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.font_picker = if this.font_picker == Some(target) {
                                None
                            } else {
                                Some(target)
                            };
                            cx.notify();
                        })),
                ),
        );
        if self.font_picker == Some(target) {
            let query = input.read(cx).text().trim().to_lowercase();
            let mut choices = div()
                .id(("font-choices", target as u32))
                .ml(px(158.))
                .max_h(px(170.))
                .overflow_y_scroll()
                .border_1()
                .border_color(rgb(0xb7bdc4))
                .bg(rgb(0xffffff));
            choices = choices.child(
                div()
                    .id(("font-system", target as u32))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .child(self.i18n.text(if target == FontTarget::Time {
                        "default_monospace"
                    } else {
                        "system_default"
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.font_input(target)
                            .update(cx, |field, cx| field.set_text("", cx));
                        this.font_picker = None;
                        cx.notify();
                    })),
            );
            for (index, name) in self
                .fonts
                .iter()
                .filter(|name| name.to_lowercase().contains(&query))
                .enumerate()
            {
                let name = name.clone();
                choices = choices.child(
                    div()
                        .id(("font-choice", index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(0xe8eff6)))
                        .child(name.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.font_input(target)
                                .update(cx, |field, cx| field.set_text(&name, cx));
                            this.font_picker = None;
                            cx.notify();
                        })),
                );
            }
            field = field.child(choices);
        }
        field
    }

    fn render_appearance_settings(&mut self, cx: &mut Context<Self>) -> Div {
        div()
            .w(px(680.))
            .p_4()
            .my_4()
            .bg(rgb(0xffffff))
            .border_1()
            .border_color(rgb(0xb7bdc4))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("appearance")),
            )
            .child(
                div()
                    .text_color(rgb(0x52606c))
                    .child(self.i18n.text("appearance_intro")),
            )
            .child(color_field(
                &self.i18n.text("member_list_background"),
                self.settings.member_list_background.clone(),
                cx,
            ))
            .child(color_field(
                &self.i18n.text("channel_log"),
                self.settings.main_log_background.clone(),
                cx,
            ))
            .child(color_field(
                &self.i18n.text("channel_log_alternate"),
                self.settings.main_log_alternate.clone(),
                cx,
            ))
            .child(color_field(
                &self.i18n.text("combined_log"),
                self.settings.sub_log_background.clone(),
                cx,
            ))
            .child(color_field(
                &self.i18n.text("combined_log_alternate"),
                self.settings.sub_log_alternate.clone(),
                cx,
            ))
            .child(
                div()
                    .id("alternate-rows")
                    .ml(px(158.))
                    .flex()
                    .gap_2()
                    .cursor_pointer()
                    .child(if self.settings.values.appearance.alternate_rows {
                        "☑"
                    } else {
                        "☐"
                    })
                    .child(self.i18n.text("alternate_rows"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let value = &mut this.settings.values.appearance.alternate_rows;
                        *value = !*value;
                        cx.notify();
                    })),
            )
            .child(self.font_field(FontTarget::MainLog, &self.i18n.text("channel_log"), cx))
            .child(self.font_field(FontTarget::SubLog, &self.i18n.text("combined_log"), cx))
            .child(self.font_field(FontTarget::Members, &self.i18n.text("member_list"), cx))
            .child(self.font_field(FontTarget::Channels, &self.i18n.text("channel_list"), cx))
            .child(self.font_field(FontTarget::Input, &self.i18n.text("draft_input"), cx))
            .child(self.font_field(FontTarget::Time, &self.i18n.text("timestamp_monospace"), cx))
            .when_some(self.feedback.clone(), |d, feedback| {
                d.child(div().text_color(rgb(0x9a4b28)).child(feedback))
            })
            .child(
                div()
                    .id("save-appearance")
                    .px_3()
                    .py_1()
                    .bg(rgb(0xcbdbea))
                    .cursor_pointer()
                    .child(self.i18n.text("save_and_apply"))
                    .on_click(cx.listener(|this, _, _, cx| this.save_settings(cx))),
            )
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = div()
            .flex()
            .gap_2()
            .child(
                div()
                    .id("connection-tab")
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .when(self.tab == SettingsTab::Connection, |d| d.bg(rgb(0xcbdbea)))
                    .child(self.i18n.text("connection"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tab = SettingsTab::Connection;
                        this.font_picker = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("appearance-tab")
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .when(self.tab == SettingsTab::Appearance, |d| d.bg(rgb(0xcbdbea)))
                    .child(self.i18n.text("appearance"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tab = SettingsTab::Appearance;
                        cx.notify();
                    })),
            );
        let panel = match self.tab {
            SettingsTab::Connection => self.render_connection_settings(cx).into_any_element(),
            SettingsTab::Appearance => self.render_appearance_settings(cx).into_any_element(),
        };
        div()
            .id("settings-screen")
            .key_context("SettingsWindow")
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .bg(rgb(0xf5f6f8))
            .text_size(px(13.))
            .text_color(rgb(0x20262d))
            .child(div().w_full().flex().justify_center().child(tabs))
            .child(div().w_full().flex().justify_center().child(panel))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::disconnect_action))
            .on_action(cx.listener(Self::toggle_debug_action))
            .on_action(cx.listener(Self::copy_diagnostics_action))
    }
}

fn settings_field(label: &str, input: Entity<TextInput>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w(px(150.)).flex_shrink_0().child(label.to_owned()))
        .child(div().flex_1().min_w_0().child(input))
}

fn color_field(label: &str, input: Entity<TextInput>, cx: &App) -> Div {
    let swatch = color_value(input.read(cx).text()).unwrap_or(0xffffff);
    settings_field(label, input).child(
        div()
            .w(px(24.))
            .h(px(24.))
            .flex_shrink_0()
            .border_1()
            .border_color(rgb(0xb7bdc4))
            .bg(rgb(swatch)),
    )
}

fn selected_font<'a>(name: &'a str, fallback: &'a str) -> &'a str {
    if name.is_empty() { fallback } else { name }
}

fn default_time_font() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(target_os = "windows")]
    {
        "Consolas"
    }
    #[cfg(target_os = "linux")]
    {
        "DejaVu Sans Mono"
    }
}

fn log_urls(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let next = ["https://", "http://"]
            .into_iter()
            .filter_map(|scheme| text[cursor..].find(scheme).map(|offset| cursor + offset))
            .min();
        let Some(start) = next else { break };
        let mut end = text[start..]
            .char_indices()
            .find(|(_, ch)| ch.is_whitespace() || "<>\"'。、".contains(*ch))
            .map(|(offset, _)| start + offset)
            .unwrap_or(text.len());
        while end > start
            && text[..end]
                .chars()
                .last()
                .is_some_and(|ch| ".,;:!?)]}」』".contains(ch))
        {
            end -= text[..end].chars().last().unwrap().len_utf8();
        }
        let candidate = &text[start..end];
        if let Ok(url) = url::Url::parse(candidate)
            && matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
        {
            found.push((start..end, url.into()));
        }
        cursor = end.max(start + 1);
    }
    found
}

fn styled_log_text(
    text: &str,
    urls: &[(std::ops::Range<usize>, String)],
    selected: Option<std::ops::Range<usize>>,
) -> StyledText {
    let mut boundaries = vec![0, text.len()];
    for (range, _) in urls {
        boundaries.extend([range.start, range.end]);
    }
    if let Some(range) = &selected {
        boundaries.extend([range.start, range.end]);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let highlights = boundaries.windows(2).filter_map(|pair| {
        let range = pair[0]..pair[1];
        let is_url = urls
            .iter()
            .any(|(url, _)| url.start <= range.start && range.end <= url.end);
        let is_selected = selected
            .as_ref()
            .is_some_and(|selection| selection.start <= range.start && range.end <= selection.end);
        (is_url || is_selected).then_some((
            range,
            HighlightStyle {
                color: is_url.then_some(rgb(0x0645ad).into()),
                underline: is_url.then_some(UnderlineStyle {
                    color: Some(rgb(0x0645ad).into()),
                    thickness: px(1.),
                    wavy: false,
                }),
                background_color: is_selected.then_some(rgb(0xcbdbea).into()),
                ..Default::default()
            },
        ))
    });
    StyledText::new(text.to_owned()).with_highlights(highlights)
}

impl Render for SettingsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_settings(cx)
    }
}

impl Render for ChatWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self.state.selection();
        let main_scroll = self.main_scroll.entry(selection).or_default();
        let main_scroll_handle = main_scroll.handle.clone();
        if main_scroll.follow_latest {
            main_scroll_handle.scroll_to_bottom();
        }
        let sub_scroll_handle = self.sub_scroll.handle.clone();
        if self.sub_scroll.follow_latest {
            sub_scroll_handle.scroll_to_bottom();
        }
        let selected = self.state.selected_channel();
        let log_id = match selection {
            Selection::Channel(id) => id.0,
            Selection::Server(id) => u32::MAX - id.0,
        };
        let border = rgb(0xb7bdc4);
        let appearance = &self.appearance;
        let main_bg = rgb(color_value(&appearance.main_log_background).unwrap_or(0xffffff));
        let main_alt = rgb(color_value(&appearance.main_log_alternate).unwrap_or(0xf2f5ff));
        let sub_bg = rgb(color_value(&appearance.sub_log_background).unwrap_or(0xf9fafb));
        let sub_alt = rgb(color_value(&appearance.sub_log_alternate).unwrap_or(0xf2f5ff));
        let time_font = selected_font(&appearance.time_font, default_time_font()).to_owned();

        // The reference layout has logs on the left and users/channels on the right.
        let mut channels = div()
            .id("channels")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .bg(rgb(0xeaf3ff))
            .when(!appearance.channel_font.is_empty(), |d| {
                d.font_family(appearance.channel_font.clone())
            })
            .border_t_1()
            .border_color(border);
        for network in self.state.networks() {
            let server_id = network.id;
            let status_mark = match self.state.status(server_id) {
                Some(ConnectionStatus::Registered) => " ●",
                Some(ConnectionStatus::Connecting | ConnectionStatus::TransportConnected) => " …",
                Some(ConnectionStatus::Disconnected(_)) => " ×",
                _ => "",
            };
            channels = channels.child(
                div()
                    .id(("server", server_id.0))
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .when(selection == Selection::Server(server_id), |d| {
                        d.bg(rgb(0xcbdbea))
                    })
                    .hover(|d| d.bg(rgb(0xdce5ee)))
                    .child(format!("{}{}", network.name, status_mark))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dispatch(Command::SelectServer(server_id), window, cx);
                    })),
            );
            for conversation in self
                .state
                .conversations()
                .iter()
                .filter(|c| c.network == network.id)
            {
                let id = conversation.id;
                let unread = self.state.is_unread(id);
                channels = channels.child(
                    div()
                        .id(("channel", id.0))
                        .pl_4()
                        .pr_2()
                        .py(px(2.))
                        .cursor_pointer()
                        .when(selection == Selection::Channel(id), |d| d.bg(rgb(0xcbdbea)))
                        .when(unread, |d| d.font_weight(FontWeight::BOLD))
                        .hover(|d| d.bg(rgb(0xdce5ee)))
                        .child(format!(
                            "{}{}",
                            if unread { "● " } else { "" },
                            conversation.name
                        ))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.dispatch(Command::SelectChannel(id), window, cx);
                        })),
                );
            }
        }

        let mut main_log = div()
            .id(("log", log_id))
            .key_context("MainLog")
            .track_focus(&self.log_focus)
            .on_action(cx.listener(Self::copy_log_selection))
            .on_action(cx.listener(Self::copy_log_selection_menu))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.finish_log_selection()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.finish_log_selection()),
            )
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&main_scroll_handle)
            .on_scroll_wheel(cx.listener(move |this, event, window, _| {
                if let Some(scroll) = this.main_scroll.get_mut(&selection) {
                    scroll.follow_latest =
                        reaches_log_bottom(&scroll.handle, event, window.line_height());
                }
            }))
            .px_2()
            .py_1()
            .bg(main_bg)
            .when(!appearance.main_log_font.is_empty(), |d| {
                d.font_family(appearance.main_log_font.clone())
            });
        let registration_incomplete = self.state.status(self.state.selected_network().id)
            != Some(&ConnectionStatus::Registered);
        if selected.is_some() && registration_incomplete {
            main_log = main_log.child(
                div()
                    .text_color(rgb(0x9a4b28))
                    .child(self.status_text(self.state.status(self.state.selected_network().id))),
            );
        }
        if selected.is_some() && registration_incomplete && !self.diagnostics.is_empty() {
            main_log = main_log.child(
                div()
                    .py_1()
                    .font_weight(FontWeight::BOLD)
                    .child(self.i18n.text("diagnostics_heading")),
            );
            main_log = main_log.children(
                self.diagnostics
                    .iter()
                    .map(|line| div().text_color(rgb(0x52606c)).child(line.clone())),
            );
        } else if self.debug_enabled
            && selected.is_some()
            && let Some(line) = self.diagnostics.last()
        {
            main_log = main_log.child(div().text_color(rgb(0x52606c)).child(line.clone()));
        }
        if let Some(channel) = selected {
            let selected_channel = channel.id;
            let log_selection = self
                .log_selection
                .filter(|selection| selection.channel == selected_channel);
            main_log =
                main_log.children(channel.messages.iter().enumerate().map(|(index, message)| {
                    let urls = log_urls(&message.text);
                    let selected_range = log_selection
                        .and_then(|selection| selection.range(index, message.text.len()));
                    let styled = styled_log_text(&message.text, &urls, selected_range);
                    let layout = styled.layout().clone();
                    let down_layout = layout.clone();
                    let move_layout = layout.clone();
                    let click_layout = layout;
                    let text_len = message.text.len();
                    div()
                        .flex()
                        .items_start()
                        .gap_1()
                        .py(px(1.))
                        .when(appearance.alternate_rows && index % 2 == 1, |d| {
                            d.bg(main_alt)
                        })
                        .child(
                            div()
                                .w(px(42.))
                                .flex_shrink_0()
                                .font_family(time_font.clone())
                                .text_color(rgb(0x747b82))
                                .child(message.time.clone()),
                        )
                        .child(
                            div()
                                .w(px(84.))
                                .flex_shrink_0()
                                .flex()
                                .text_color(rgb(0x315b83))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(message.sender.clone()),
                                )
                                .child(":"),
                        )
                        .child(
                            div()
                                .id(("message-text", index))
                                .flex_1()
                                .min_w_0()
                                .cursor(CursorStyle::IBeam)
                                .child(styled)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        let byte = down_layout
                                            .index_for_position(event.position)
                                            .unwrap_or_else(|index| index)
                                            .min(text_len);
                                        this.start_log_selection(
                                            selected_channel,
                                            index,
                                            byte,
                                            window,
                                            cx,
                                        );
                                    }),
                                )
                                .on_mouse_move(cx.listener(
                                    move |this, event: &MouseMoveEvent, _, cx| {
                                        let byte = move_layout
                                            .index_for_position(event.position)
                                            .unwrap_or_else(|index| index)
                                            .min(text_len);
                                        this.extend_log_selection(
                                            selected_channel,
                                            index,
                                            byte,
                                            cx,
                                        );
                                    },
                                ))
                                .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                                    if event.click_count() == 2 {
                                        let byte = click_layout
                                            .index_for_position(event.position())
                                            .unwrap_or_else(|index| index);
                                        if let Some((_, url)) =
                                            urls.iter().find(|(range, _)| range.contains(&byte))
                                        {
                                            cx.open_url(url);
                                        }
                                    }
                                })),
                        )
                }));
        } else {
            let network = self.state.selected_network();
            main_log = main_log.child(self.status_text(self.state.status(network.id)));
            if (self.debug_enabled || registration_incomplete) && !self.diagnostics.is_empty() {
                main_log = main_log.child(
                    div()
                        .py_1()
                        .font_weight(FontWeight::BOLD)
                        .child(self.i18n.text("diagnostics_heading")),
                );
                main_log = main_log.children(
                    self.diagnostics
                        .iter()
                        .map(|line| div().text_color(rgb(0x52606c)).child(line.clone())),
                );
            }
            main_log = main_log.children(
                self.state
                    .server_messages(network.id)
                    .iter()
                    .enumerate()
                    .map(|(index, message)| {
                        div()
                            .flex()
                            .gap_1()
                            .py(px(1.))
                            .when(appearance.alternate_rows && index % 2 == 1, |d| {
                                d.bg(main_alt)
                            })
                            .child(
                                div()
                                    .w(px(42.))
                                    .flex_shrink_0()
                                    .font_family(time_font.clone())
                                    .text_color(rgb(0x747b82))
                                    .child(message.time.clone()),
                            )
                            .child(div().flex_1().min_w_0().child(message.text.clone()))
                    }),
            );
        }

        let mut other_messages: Vec<_> = self
            .state
            .conversations()
            .iter()
            .filter(|conversation| Some(conversation.id) != selected.map(|c| c.id))
            .flat_map(|conversation| {
                conversation.messages.iter().map(move |message| {
                    (
                        conversation.id,
                        conversation.network,
                        conversation.name.as_str(),
                        message,
                    )
                })
            })
            .collect();
        other_messages.sort_by_key(|(_, _, _, message)| message.sequence);
        let sub_log = div()
            .id("sub-log")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&sub_scroll_handle)
            .on_scroll_wheel(cx.listener(|this, event, window, _| {
                this.sub_scroll.follow_latest =
                    reaches_log_bottom(&this.sub_scroll.handle, event, window.line_height());
            }))
            .px_2()
            .py_1()
            .bg(sub_bg)
            .when(!appearance.sub_log_font.is_empty(), |d| {
                d.font_family(appearance.sub_log_font.clone())
            })
            .children(other_messages.into_iter().enumerate().map(
                |(index, (id, network_id, channel, message))| {
                    let network = self
                        .state
                        .networks()
                        .iter()
                        .find(|n| n.id == network_id)
                        .unwrap();
                    div()
                        .id(("sub-message", index))
                        .flex()
                        .gap_2()
                        .py(px(1.))
                        .when(appearance.alternate_rows && index % 2 == 1, |d| {
                            d.bg(sub_alt)
                        })
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(0xe8eff6)))
                        .child(
                            div()
                                .w(px(42.))
                                .flex_shrink_0()
                                .font_family(time_font.clone())
                                .text_color(rgb(0x747b82))
                                .child(message.time.clone()),
                        )
                        .child(
                            div()
                                .w(px(162.))
                                .flex_shrink_0()
                                .flex()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_color(rgb(0x315b83))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(channel.to_owned()),
                                )
                                .child(
                                    div()
                                        .max_w(px(90.))
                                        .min_w_0()
                                        .flex_shrink_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(format!(
                                            " [{}]",
                                            network.name.split_whitespace().next().unwrap_or("")
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(format!("{}: {}", message.sender, message.text)),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.dispatch(Command::SelectChannel(id), window, cx);
                        }))
                },
            ));

        let members = div()
            .id("members")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .bg(rgb(
                color_value(&appearance.member_list_background).unwrap_or(0xffffff)
            ))
            .when(!appearance.member_font.is_empty(), |d| {
                d.font_family(appearance.member_font.clone())
            })
            .children(
                selected
                    .into_iter()
                    .flat_map(|channel| channel.members.iter())
                    .map(|nick| div().px_2().py(px(1.)).child(nick.clone())),
            );

        let main_pane = div().flex().flex_col().flex_1().min_h_0().child(main_log);
        let sub_pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(border)
            .child(sub_log);
        let editor = div()
            .flex()
            .items_center()
            .h(px(38.))
            .flex_shrink_0()
            .px_1()
            .border_t_1()
            .border_b_1()
            .border_color(border)
            .bg(rgb(0xffffff))
            .when(!appearance.input_font.is_empty(), |d| {
                d.font_family(appearance.input_font.clone())
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.inputs[&selection].clone()),
            )
            .when_some(self.feedback.clone(), |d, feedback| {
                d.child(div().text_color(rgb(0x9a4b28)).child(feedback))
            });
        let left = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(main_pane)
            .child(editor)
            .child(sub_pane);
        let right = div()
            .flex()
            .flex_col()
            .w(px(240.))
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(border)
            .child(members)
            .child(channels);

        div()
            .key_context("ChatWindow")
            .size_full()
            .flex()
            .text_size(px(13.))
            .line_height(px(20.))
            .text_color(rgb(0x20262d))
            .bg(rgb(0xffffff))
            .on_action(cx.listener(Self::navigate))
            .on_action(cx.listener(Self::complete_nickname))
            .on_action(cx.listener(Self::send_message))
            .on_action(cx.listener(Self::notice))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::disconnect_action))
            .on_action(cx.listener(Self::toggle_debug))
            .on_action(cx.listener(Self::copy_diagnostics))
            .child(left)
            .child(right)
            .into_any_element()
    }
}

fn navigation_binding(key: &str, command: Command) -> KeyBinding {
    KeyBinding::new(key, Navigate { command }, Some("ChatWindow"))
}

fn app_menus(debug_enabled: bool, i18n: &Localizer) -> Vec<Menu> {
    let mut app_items = vec![
        MenuItem::action(i18n.text("menu_settings"), OpenSettings),
        MenuItem::separator(),
    ];
    #[cfg(target_os = "macos")]
    app_items.push(MenuItem::os_submenu(
        i18n.text("menu_services"),
        SystemMenuType::Services,
    ));
    app_items.push(MenuItem::action(i18n.text("menu_quit"), Quit));
    vec![
        Menu {
            name: "CayenChat".into(),
            items: app_items,
        },
        Menu {
            name: i18n.text("menu_connection").into(),
            items: vec![
                MenuItem::action(i18n.text("menu_settings"), OpenSettings),
                MenuItem::action(i18n.text("disconnect"), Disconnect),
            ],
        },
        Menu {
            name: i18n.text("menu_edit").into(),
            items: vec![
                MenuItem::action(i18n.text("menu_undo"), input::Undo),
                MenuItem::action(i18n.text("menu_redo"), input::Redo),
                MenuItem::separator(),
                MenuItem::action(i18n.text("menu_cut"), input::Cut),
                MenuItem::action(i18n.text("menu_copy"), input::Copy),
                MenuItem::action(i18n.text("menu_paste"), input::Paste),
                MenuItem::action(i18n.text("menu_select_all"), input::SelectAll),
            ],
        },
        Menu {
            name: i18n.text("menu_view").into(),
            items: vec![
                MenuItem::action(
                    if debug_enabled {
                        i18n.text("menu_hide_diagnostics")
                    } else {
                        i18n.text("menu_show_diagnostics")
                    },
                    ToggleDebug,
                ),
                MenuItem::action(i18n.text("menu_copy_diagnostics"), CopyDiagnostics),
            ],
        },
        Menu {
            name: i18n.text("menu_window").into(),
            items: vec![MenuItem::action(i18n.text("menu_settings"), OpenSettings)],
        },
    ]
}

fn shortcut_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("tab", CompleteNickname, Some("TextInput")),
        KeyBinding::new("enter", SendMessage, Some("TextInput")),
        KeyBinding::new("ctrl-enter", Notice, Some("TextInput")),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-shift-d", ToggleDebug, None),
        KeyBinding::new("secondary-shift-l", CopyDiagnostics, None),
        KeyBinding::new("secondary-c", CopyLogSelection, Some("MainLog")),
        navigation_binding("ctrl-tab", Command::NextUnreadChannel),
        navigation_binding("ctrl-shift-tab", Command::PreviousUnreadChannel),
        KeyBinding::new("secondary-q", Quit, None),
    ];

    #[cfg(target_os = "macos")]
    bindings.extend([
        navigation_binding("alt-space", Command::NextUnreadChannel),
        navigation_binding("alt-shift-space", Command::PreviousUnreadChannel),
        navigation_binding("alt-tab", Command::PreviousSelectedChannel),
        navigation_binding("cmd-up", Command::PreviousActiveChannel),
        navigation_binding("cmd-down", Command::NextActiveChannel),
        navigation_binding("cmd-alt-up", Command::PreviousActiveChannel),
        navigation_binding("cmd-alt-down", Command::NextActiveChannel),
        navigation_binding("cmd-{", Command::PreviousActiveChannel),
        navigation_binding("cmd-}", Command::NextActiveChannel),
        navigation_binding("ctrl-up", Command::PreviousChannel),
        navigation_binding("ctrl-down", Command::NextChannel),
        navigation_binding("cmd-alt-left", Command::PreviousActiveServer),
        navigation_binding("cmd-alt-right", Command::NextActiveServer),
        navigation_binding("ctrl-left", Command::PreviousServer),
        navigation_binding("ctrl-right", Command::NextServer),
    ]);

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    bindings.extend([
        navigation_binding("alt-left", Command::PreviousSelectedChannel),
        navigation_binding("ctrl-pageup", Command::PreviousActiveChannel),
        navigation_binding("ctrl-pagedown", Command::NextActiveChannel),
        navigation_binding("alt-up", Command::PreviousChannel),
        navigation_binding("alt-down", Command::NextChannel),
        navigation_binding("ctrl-alt-pageup", Command::PreviousActiveServer),
        navigation_binding("ctrl-alt-pagedown", Command::NextActiveServer),
        navigation_binding("alt-pageup", Command::PreviousServer),
        navigation_binding("alt-pagedown", Command::NextServer),
    ]);

    for index in 0..10 {
        let digit = (index + 1) % 10;
        #[cfg(target_os = "macos")]
        {
            bindings.push(navigation_binding(
                &format!("cmd-{digit}"),
                Command::SelectChannelAt(index),
            ));
            bindings.push(navigation_binding(
                &format!("cmd-ctrl-{digit}"),
                Command::SelectServerAt(index),
            ));
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            bindings.push(navigation_binding(
                &format!("ctrl-{digit}"),
                Command::SelectChannelAt(index),
            ));
            bindings.push(navigation_binding(
                &format!("ctrl-alt-{digit}"),
                Command::SelectServerAt(index),
            ));
        }
    }
    bindings
}

fn main() {
    Application::new().run(|cx: &mut App| {
        input::bind_keys(cx);
        cx.bind_keys(shortcut_bindings());
        cx.set_menus(app_menus(false, &Localizer::new(Language::System)));
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(960.), px(600.)), cx);
        let chat_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(720.), px(420.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("CayenChat".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                move |window, cx| cx.new(|cx| ChatWindow::new(window, cx)),
            )
            .expect("could not open CayenChat window");
        cx.activate(true);
        let needs_settings = chat_window
            .update(cx, |chat, window, cx| {
                cx.set_menus(app_menus(false, &chat.i18n));
                match chat.startup_connection.take() {
                    Some(Ok(config)) => chat.apply_connection(config, window, cx).is_err(),
                    Some(Err(error)) => {
                        chat.feedback =
                            Some(chat.i18n.format("startup_invalid", &[("error", &error)]));
                        cx.notify();
                        true
                    }
                    None => true,
                }
            })
            .expect("could not initialize the chat window");
        if needs_settings {
            chat_window
                .update(cx, |chat, window, cx| {
                    chat.open_settings(&OpenSettings, window, cx)
                })
                .expect("could not open the initial settings window");
        }
    });
}

#[cfg(test)]
mod log_tests {
    use super::{LogPosition, LogSelection, log_urls};
    use cayenchat_model::ConversationId;

    #[test]
    fn finds_only_web_urls_without_sentence_punctuation() {
        let text =
            "see https://example.org/a?q=1, and http://example.jp/path。 ftp://example.org/x";
        let urls = log_urls(text);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].1, "https://example.org/a?q=1");
        assert_eq!(urls[1].1, "http://example.jp/path");
        assert_eq!(&text[urls[0].0.clone()], urls[0].1);
    }

    #[test]
    fn log_selection_spans_partial_messages() {
        let selection = LogSelection {
            channel: ConversationId(1),
            anchor: LogPosition { row: 2, byte: 3 },
            cursor: LogPosition { row: 0, byte: 2 },
        };
        assert_eq!(selection.range(0, 5), Some(2..5));
        assert_eq!(selection.range(1, 5), Some(0..5));
        assert_eq!(selection.range(2, 5), Some(0..3));
        assert_eq!(selection.range(3, 5), None);
    }
}

#[cfg(test)]
mod startup_tests {
    use super::startup_connection_config;
    use cayenchat_storage::Settings;

    #[test]
    fn startup_uses_only_saved_credentials_when_enabled() {
        let mut settings = Settings {
            nickname: "alice".into(),
            ..Settings::default()
        };
        assert!(startup_connection_config(&settings).is_none());

        settings.connect_on_startup = true;
        settings.sasl_enabled = true;
        settings.sasl_username = "account".into();
        settings.selected_profile_mut().use_tls = true;
        assert!(startup_connection_config(&settings).unwrap().is_err());

        settings.selected_profile_mut().sasl_password = Some("secret".into());
        assert!(startup_connection_config(&settings).unwrap().is_err());
        settings.selected_profile_mut().remember_passwords = true;
        let config = startup_connection_config(&settings).unwrap().unwrap();
        assert_eq!(config.host, "irc.ircnet.ne.jp");
        assert_eq!(config.sasl.unwrap().password, "secret");
    }
}
