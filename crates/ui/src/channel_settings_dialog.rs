// SPDX-License-Identifier: GPL-3.0-only
//! The settings of one channel (#680, #781, #950): joining it automatically,
//! its encodings and numeric character reference mode, and the look of its
//! log, in a window of its own that the chat window (channel menu) and the
//! settings window (auto-join list) both open. The window saves by itself
//! after every edit, so the chat window shows each change while it is
//! adjusted; there is no Save button. The values are written with
//! [`save_channel_choices`] (built from [`apply_charsets`] and
//! [`apply_auto_join`]) through [`ChatWindow::apply_channel_settings`]. Saving
//! never joins or parts the channel.

mod wallpaper_crop;
mod wallpaper_preview;

use std::time::Duration;

use crate::wallpaper_crop::CropState;

use cayenchat_model::{display, names::CaseMapping};
use cayenchat_storage::{
    AutoJoinEntry, ChannelLook, CharRefMode, DEFAULT_BACKGROUND_OPACITY, DEFAULT_WALLPAPER_BLUR,
    MAX_FONT_SIZE, MAX_WALLPAPER_BLUR, MAX_WALLPAPER_PATH_BYTES, MIN_FONT_SIZE, ServerProfile,
    Settings, TextEncoding, WallpaperFit, color_value, format_auto_join,
};
use gpui::{prelude::*, *};

use crate::{
    ChatWindow, FocusNextField, FocusPreviousField,
    channel_look::{background_opacity_of, blur_of},
    decorations,
    input::TextInput,
    localization::Localizer,
    os_text::WindowOsText,
    scrollbar, settings_file, settings_theme,
    settings_window::settings_field,
    slider::{self, SliderColors, TrackBounds},
};

/// Tallest the open encoding list gets before it scrolls.
const MENU_HEIGHT: Pixels = px(176.);
/// The pause after the last edit before it is saved, so a drag or typing is
/// written once.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EncodingSide {
    Decode,
    Encode,
}

/// What the dialog holds for a channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Choices {
    pub(crate) enabled: bool,
    pub(crate) decode: Option<TextEncoding>,
    pub(crate) encode: Option<TextEncoding>,
    pub(crate) char_refs: Option<CharRefMode>,
}

impl Choices {
    /// What `profile` holds for `channel` now.
    pub(crate) fn of(profile: &ServerProfile, channel: &str, mapping: CaseMapping) -> Self {
        let same = |left: &str, right: &str| mapping.same(left, right);
        let saved = profile.channel_charset(channel, same);
        Self {
            enabled: profile
                .auto_join_entries()
                .iter()
                .any(|entry| entry.enabled && same(&entry.name, channel)),
            decode: saved.and_then(|entry| entry.decode),
            encode: saved.and_then(|entry| entry.encode),
            char_refs: saved.and_then(|entry| entry.char_refs),
        }
    }
}

/// The choices of a numeric character reference mode, with the label key of
/// each. The channel dialog puts its "follow the server" choice before them.
pub(crate) const CHAR_REF_CHOICES: [(CharRefMode, &str); 3] = [
    (CharRefMode::Auto, "char_refs_auto"),
    (CharRefMode::Iso2022Jp, "char_refs_iso2022jp"),
    (CharRefMode::Off, "char_refs_off"),
];

/// Whether a UTF8ONLY server accepts these encodings.
pub(crate) fn channel_encodings_allowed(
    utf8_only: bool,
    decode: Option<TextEncoding>,
    encode: Option<TextEncoding>,
) -> bool {
    !utf8_only
        || [decode, encode]
            .into_iter()
            .flatten()
            .all(|encoding| encoding == TextEncoding::Utf8)
}

/// Writes the encodings and the reference mode of `channel` into `profile`.
/// An error leaves the profile as it was.
pub(crate) fn apply_charsets(
    profile: &mut ServerProfile,
    channel: &str,
    mapping: CaseMapping,
    choices: Choices,
    utf8_only: bool,
    i18n: &Localizer,
) -> Result<(), String> {
    if !channel_encodings_allowed(utf8_only, choices.decode, choices.encode) {
        return Err(i18n.text("charset_utf8only"));
    }
    profile.set_channel_charset(
        channel,
        choices.decode,
        choices.encode,
        choices.char_refs,
        |left, right| mapping.same(left, right),
    )
}

/// Writes the auto-join choice of `channel` into the profile's channel list
/// and returns the list's text. A channel that was never listed is not added
/// just to be off.
pub(crate) fn apply_auto_join(
    profile: &mut ServerProfile,
    channel: &str,
    mapping: CaseMapping,
    enabled: bool,
) -> String {
    let mut entries = profile.auto_join_entries();
    let found = entries
        .iter_mut()
        .filter(|entry| mapping.same(&entry.name, channel))
        .map(|entry| entry.enabled = enabled)
        .count();
    if found == 0 && enabled {
        entries.push(AutoJoinEntry {
            name: channel.to_owned(),
            enabled: true,
        });
    }
    let text = format_auto_join(&entries);
    profile.channels = text.trim().to_owned();
    text
}

/// Applies the dialog's `choices` for `channel` to the profile `profile_id`
/// of `settings`: the encodings and the reference mode, then the auto-join
/// choice. Returns a copy of the updated profile. It reads and writes no file
/// and touches no host copy; an error leaves `settings` as it was.
#[allow(clippy::too_many_arguments)] // The dialog's values and where they are written.
pub(crate) fn save_channel_choices(
    settings: &mut Settings,
    profile_id: &str,
    channel: &str,
    mapping: CaseMapping,
    choices: Choices,
    mut look: ChannelLook,
    utf8_only: bool,
    i18n: &Localizer,
) -> Result<ServerProfile, String> {
    look.server = profile_id.to_owned();
    look.channel = channel.to_owned();
    let old_looks = settings.appearance.channel_looks.clone();
    settings
        .appearance
        .set_channel_look(look, |left, right| mapping.same(left, right))?;
    let Some(profile) = settings.profile_mut(profile_id) else {
        settings.appearance.channel_looks = old_looks;
        return Err(format!("unknown server profile: {profile_id}"));
    };
    if let Err(error) = apply_charsets(profile, channel, mapping, choices, utf8_only, i18n) {
        settings.appearance.channel_looks = old_looks;
        return Err(error);
    }
    apply_auto_join(profile, channel, mapping, choices.enabled);
    Ok(profile.clone())
}

/// `latest` with the fields `new` changed from `base`.
fn merge_choices(latest: Choices, base: Choices, new: Choices) -> Choices {
    Choices {
        enabled: if new.enabled != base.enabled {
            new.enabled
        } else {
            latest.enabled
        },
        decode: if new.decode != base.decode {
            new.decode
        } else {
            latest.decode
        },
        encode: if new.encode != base.encode {
            new.encode
        } else {
            latest.encode
        },
        char_refs: if new.char_refs != base.char_refs {
            new.char_refs
        } else {
            latest.char_refs
        },
    }
}

/// `latest` with the fields `new` changed from `base`.
pub(crate) fn merge_look(
    latest: Option<&ChannelLook>,
    base: &ChannelLook,
    new: &ChannelLook,
) -> ChannelLook {
    let mut merged = latest.cloned().unwrap_or_default();
    macro_rules! take {
        ($($field:ident),*) => {
            $(if new.$field != base.$field {
                merged.$field = new.$field.clone();
            })*
        };
    }
    take!(
        font_size,
        background,
        alternate,
        text,
        wallpaper,
        wallpaper_blur,
        background_opacity,
        wallpaper_fit,
        no_wallpaper
    );
    merged
}

/// The fields of the channel log look. A text field left empty follows the
/// global appearance; the blur, the background opacity and the fit always
/// hold a value (their defaults at first), which is what is shown and used.
struct LookFields {
    font_size: Entity<TextInput>,
    background: Entity<TextInput>,
    alternate: Entity<TextInput>,
    text: Entity<TextInput>,
    wallpaper: Entity<TextInput>,
    /// Blur sigma, 0 to [`MAX_WALLPAPER_BLUR`].
    blur: u8,
    /// Whether the blur is the channel's own, also at the default value or 0:
    /// an inherited one follows the standard wallpaper's.
    blur_own: bool,
    /// Opacity in percent of the background color over the wallpaper.
    opacity: u8,
    /// Whether the opacity is the channel's own, also at the default value:
    /// an inherited one follows the standard wallpaper's (#962).
    opacity_own: bool,
    /// The channel's own fit, also an explicit `Cover`; `None` follows the
    /// standard wallpaper's.
    fit: Option<WallpaperFit>,
    blur_track: TrackBounds,
    opacity_track: TrackBounds,
    wallpaper_choice: WallpaperChoice,
}

/// What the channel does about the wallpaper (#962).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WallpaperChoice {
    /// Follow the standard wallpaper of the channel log.
    Inherit,
    /// None, even where the standard has one.
    Off,
    /// The picture named in the file field.
    Own,
}

impl WallpaperChoice {
    const ALL: [(Self, &'static str); 3] = [
        (Self::Inherit, "channel_look_wallpaper_inherit"),
        (Self::Off, "channel_look_wallpaper_off"),
        (Self::Own, "channel_look_wallpaper_own"),
    ];
}

/// The wallpaper fits, with their label keys.
const FIT_CHOICES: [(WallpaperFit, &str); 4] = [
    (WallpaperFit::Stretch, "channel_look_fit_stretch"),
    (WallpaperFit::Cover, "channel_look_fit_cover"),
    (WallpaperFit::Contain, "channel_look_fit_contain"),
    (WallpaperFit::Center, "channel_look_fit_center"),
];

impl LookFields {
    fn new(look: Option<&ChannelLook>, i18n: &Localizer, cx: &mut App) -> Self {
        let unset = i18n.text("channel_look_unset");
        let field = |value: String, cx: &mut App| {
            cx.new(|cx| TextInput::new_settings_field(&unset, &value, false, cx))
        };
        let look = look.cloned().unwrap_or_default();
        let color = |value: Option<String>| value.unwrap_or_default();
        let wallpaper_choice = if look.no_wallpaper {
            WallpaperChoice::Off
        } else if look.wallpaper.is_some() {
            WallpaperChoice::Own
        } else {
            WallpaperChoice::Inherit
        };
        Self {
            wallpaper_choice,
            font_size: field(
                match look.font_size {
                    0 => String::new(),
                    size => size.to_string(),
                },
                cx,
            ),
            blur: blur_of(&look),
            blur_own: look.wallpaper_blur.is_some(),
            opacity: background_opacity_of(&look),
            opacity_own: look.background_opacity.is_some(),
            fit: look.wallpaper_fit,
            background: field(color(look.background), cx),
            alternate: field(color(look.alternate), cx),
            text: field(color(look.text), cx),
            wallpaper: field(look.wallpaper.unwrap_or_default(), cx),
            blur_track: TrackBounds::default(),
            opacity_track: TrackBounds::default(),
        }
    }

    /// The text fields in the order they are shown.
    fn fields(&self) -> [&Entity<TextInput>; 5] {
        [
            &self.font_size,
            &self.background,
            &self.alternate,
            &self.text,
            &self.wallpaper,
        ]
    }

    /// Back to the defaults: the text fields empty, the sliders and the fit
    /// at their default values, the wallpaper inherited.
    fn clear(&mut self, cx: &mut App) {
        for field in self.fields() {
            field.update(cx, |field, cx| field.set_text("", cx));
        }
        self.blur = DEFAULT_WALLPAPER_BLUR;
        self.blur_own = false;
        self.opacity = DEFAULT_BACKGROUND_OPACITY;
        self.opacity_own = false;
        self.fit = None;
        self.wallpaper_choice = WallpaperChoice::Inherit;
    }

    /// The look the fields hold, or why it cannot be used. A value equal to
    /// its default is stored as unset, so it follows the default later too;
    /// the opacity, the blur and the fit are the exception (`opacity_own`,
    /// `blur_own`, `fit`): a channel can set them equal to, or 0 under, a
    /// standard one.
    fn read(&self, i18n: &Localizer, cx: &App) -> Result<ChannelLook, String> {
        let text = |field: &Entity<TextInput>| field.read(cx).text().trim().to_owned();
        let color = |field: &Entity<TextInput>, key: &str| -> Result<Option<String>, String> {
            let value = text(field);
            if value.is_empty() {
                Ok(None)
            } else if color_value(&value).is_some() {
                Ok(Some(value.to_ascii_uppercase()))
            } else {
                Err(i18n.format("channel_look_color_invalid", &[("label", &i18n.text(key))]))
            }
        };
        let size = text(&self.font_size);
        let font_size = if size.is_empty() {
            0
        } else {
            size.parse::<u16>()
                .ok()
                .filter(|size| (MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(size))
                .ok_or_else(|| {
                    i18n.format(
                        "channel_look_size_invalid",
                        &[
                            ("min", &MIN_FONT_SIZE.to_string()),
                            ("max", &MAX_FONT_SIZE.to_string()),
                        ],
                    )
                })?
        };
        // A file typed into the field is the channel's own picture even
        // when "inherit" is still marked; "no wallpaper" ignores the field.
        let typed = text(&self.wallpaper);
        let choice = match self.wallpaper_choice {
            WallpaperChoice::Inherit if !typed.is_empty() => WallpaperChoice::Own,
            choice => choice,
        };
        let wallpaper = if choice == WallpaperChoice::Own {
            typed
        } else {
            String::new()
        };
        if wallpaper.len() > MAX_WALLPAPER_PATH_BYTES {
            return Err(i18n.text("channel_look_wallpaper_long"));
        }
        if choice == WallpaperChoice::Own && wallpaper.is_empty() {
            return Err(i18n.text("channel_look_wallpaper_empty"));
        }
        if !wallpaper.is_empty() && !std::path::Path::new(&wallpaper).is_file() {
            return Err(i18n.text("channel_look_wallpaper_missing"));
        }
        Ok(ChannelLook {
            font_size,
            background: color(&self.background, "channel_look_background")?,
            alternate: color(&self.alternate, "channel_look_alternate")?,
            text: color(&self.text, "channel_look_text")?,
            wallpaper: (!wallpaper.is_empty()).then_some(wallpaper),
            wallpaper_blur: self.blur_own.then_some(self.blur),
            background_opacity: self.opacity_own.then_some(self.opacity),
            wallpaper_fit: self.fit,
            no_wallpaper: choice == WallpaperChoice::Off,
            ..ChannelLook::default()
        })
    }
}

/// The "inherit" radio plus one per `CHAR_REF_CHOICES` entry.
const CHAR_REF_COUNT: usize = 4;
/// One button per `FIT_CHOICES` entry.
const FIT_COUNT: usize = 4;

/// The focus handles of the dialog's controls.
struct Controls {
    auto_join: FocusHandle,
    decode: FocusHandle,
    encode: FocusHandle,
    char_refs: [FocusHandle; CHAR_REF_COUNT],
    wallpaper_choice: [FocusHandle; 3],
    /// The range editor's buttons, reachable while it is open.
    crop_use: FocusHandle,
    crop_whole: FocusHandle,
    crop_cancel: FocusHandle,
    /// The miniature's "choose an image" button.
    pub(super) choose: FocusHandle,
    blur: FocusHandle,
    opacity: FocusHandle,
    fit: [FocusHandle; FIT_COUNT],
    reset: FocusHandle,
}

impl Controls {
    fn new(cx: &mut App) -> Self {
        let handle = || cx.focus_handle().tab_stop(true);
        Self {
            auto_join: handle(),
            decode: handle(),
            encode: handle(),
            char_refs: std::array::from_fn(|_| handle()),
            wallpaper_choice: std::array::from_fn(|_| handle()),
            crop_use: handle(),
            crop_whole: handle(),
            crop_cancel: handle(),
            choose: handle(),
            blur: handle(),
            opacity: handle(),
            fit: std::array::from_fn(|_| handle()),
            reset: handle(),
        }
    }

    /// The controls in Tab order, each with the body child to scroll into
    /// view when it takes the focus. The body's children are the hint, then
    /// one per control group, the last being the log look.
    fn tab_order(
        &self,
        look: &LookFields,
        crop_open: bool,
        cx: &App,
    ) -> Vec<(FocusHandle, Option<usize>)> {
        let mut order = vec![
            (self.auto_join.clone(), Some(1)),
            (self.decode.clone(), Some(2)),
            (self.encode.clone(), Some(3)),
        ];
        order.extend(
            self.char_refs
                .iter()
                .map(|handle| (handle.clone(), Some(4))),
        );
        let field = |input: &Entity<TextInput>| (input.read(cx).focus_handle(cx), Some(5));
        let fields = look.fields();
        order.extend(fields[..4].iter().map(|input| field(input)));
        order.extend(
            self.wallpaper_choice
                .iter()
                .map(|handle| (handle.clone(), Some(5))),
        );
        if crop_open {
            order.extend(
                [&self.crop_use, &self.crop_whole, &self.crop_cancel]
                    .map(|handle| (handle.clone(), Some(5))),
            );
        }
        order.extend([
            (self.choose.clone(), Some(5)),
            (self.blur.clone(), Some(5)),
            (self.opacity.clone(), Some(5)),
        ]);
        order.extend(self.fit.iter().map(|handle| (handle.clone(), Some(5))));
        order.push((self.reset.clone(), None));
        order
    }
}

pub(crate) struct ChannelSettingsView {
    /// The chat window the values are saved through and shown in.
    owner: WindowHandle<ChatWindow>,
    profile_id: String,
    channel: String,
    mapping: CaseMapping,
    /// The server's name as shown under the title.
    server: String,
    /// Whether the server allows only UTF-8, read when the window opened so
    /// that quitting needs no other window.
    utf8_only: bool,
    pub(crate) choices: Choices,
    /// The encoding list that is open, with the option the keyboard is on
    /// (0 is "follow the server").
    pub(crate) open_list: Option<EncodingSide>,
    highlight: usize,
    pub(crate) error: Option<String>,
    controls: Controls,
    look: LookFields,
    /// The wallpaper image being chosen and its range.
    crop: CropState,
    /// The miniature's picture, loaded like the chat window's.
    miniature: crate::channel_look::Wallpaper,
    /// What was last written (or read), so an edit that changes nothing
    /// writes nothing.
    saved: Option<(Choices, ChannelLook)>,
    /// What the window last wrote or opened with, even if it could not be
    /// read as a look: the saved edit is the difference from it, so a value
    /// changed elsewhere meanwhile is not written back over.
    base: (Choices, ChannelLook),
    /// The pause before the latest edit is saved.
    autosave: Option<Task<()>>,
    /// Redraws and saves when a text field changes.
    _subscriptions: Vec<Subscription>,
    window: AnyWindowHandle,
    /// The first control takes the focus on the first draw.
    focus_pending: bool,
    body_scroll: ScrollHandle,
    menu_scroll: ScrollHandle,
    i18n: Localizer,
}

impl ChannelSettingsView {
    #[allow(clippy::too_many_arguments)] // What the window shows and where it saves.
    pub(crate) fn new(
        owner: WindowHandle<ChatWindow>,
        profile_id: String,
        channel: &str,
        mapping: CaseMapping,
        server: String,
        utf8_only: bool,
        choices: Choices,
        look: Option<&ChannelLook>,
        i18n: Localizer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let opened_look = look.cloned().unwrap_or_default();
        let look = LookFields::new(look, &i18n, cx);
        let mut subscriptions: Vec<Subscription> = look
            .fields()
            .into_iter()
            .map(|field| cx.observe(field, |this, _, cx| this.edited(cx)))
            .collect();
        let saved = look.read(&i18n, cx).ok().map(|look| (choices, look));
        let base = saved.clone().unwrap_or((choices, opened_look));
        subscriptions.push(cx.on_app_quit(|this, cx| {
            this.write_on_quit(cx);
            async {}
        }));
        // Leaving saves an edit still waiting out its pause. This runs inside
        // the platform's close event, which holds the app: the windows are
        // updated only after it returns.
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let job = view
                .update(cx, |this, _| {
                    this.autosave = None;
                })
                .ok()
                .and_then(|()| view.read_with(cx, |this, cx| this.job(cx)).ok())
                .and_then(Result::ok)
                .flatten();
            if let Some(job) = job {
                // A task, not `defer`: a deferred call still runs before the
                // platform's event handler returns.
                cx.spawn(async move |cx| {
                    let _ = cx.update(|cx| {
                        if !matches!(job.run(cx), Ok(Ok(()))) {
                            let _ = job
                                .owner
                                .update(cx, |chat, _, cx| chat.discard_look_preview(cx));
                        }
                    });
                })
                .detach();
            }
            true
        });
        Self {
            owner,
            profile_id,
            look,
            channel: channel.to_owned(),
            mapping,
            server,
            utf8_only,
            choices,
            open_list: None,
            highlight: 0,
            error: None,
            controls: Controls::new(cx),
            crop: CropState::new(),
            miniature: Default::default(),
            saved,
            base,
            autosave: None,
            _subscriptions: subscriptions,
            window: window.window_handle(),
            focus_pending: true,
            body_scroll: ScrollHandle::new(),
            menu_scroll: ScrollHandle::new(),
            i18n,
        }
    }

    /// The log look the window holds, or why it cannot be saved.
    pub(crate) fn look(&self, cx: &App) -> Result<ChannelLook, String> {
        self.look.read(&self.i18n, cx)
    }

    /// Something was edited: saves it after a short pause, and shows at once
    /// the reason when it cannot be.
    fn edited(&mut self, cx: &mut Context<Self>) {
        self.preview(cx);
        let window = self.window;
        self.autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_DELAY).await;
            let _ = cx.update_window(window, |_, window, cx| {
                this.update(cx, |this, cx| this.save_now(window, cx))
            });
        }));
        cx.notify();
    }

    /// Shows the look the window holds in the chat window at once, without
    /// waiting for the pause; the pause only spares the file.
    fn preview(&mut self, cx: &mut Context<Self>) {
        let Ok(look) = self.look(cx) else {
            return;
        };
        let (profile_id, channel, mapping, base) = (
            self.profile_id.clone(),
            self.channel.clone(),
            self.mapping,
            self.base.1.clone(),
        );
        let _ = self.owner.update(cx, |chat, _, cx| {
            chat.preview_channel_look(&profile_id, &channel, mapping, &base, &look, cx);
        });
    }

    /// Writes what the window holds through the chat window, which shows it
    /// at once. A value that cannot be used is reported and not written; the
    /// last good state stays saved.
    pub(crate) fn save_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.autosave = None;
        let job = match self.job(cx) {
            Ok(Some(job)) => job,
            Ok(None) => {
                self.error = None;
                cx.notify();
                return;
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let current = (job.choices, job.look.clone());
        let result = job.run(cx);
        match result {
            Ok(Ok(())) => {
                self.base = current.clone();
                self.saved = Some(current);
                self.error = None;
            }
            Ok(Err(error)) => {
                self.error = Some(error);
                // What was shown ahead of the file goes back to the file.
                let _ = self
                    .owner
                    .update(cx, |chat, _, cx| chat.discard_look_preview(cx));
            }
            // The chat window is gone: nothing is left to save to.
            Err(_) => window.remove_window(),
        }
        cx.notify();
    }

    /// What would be written now: `None` when it is what was saved already,
    /// the reason when a value cannot be used.
    fn job(&self, cx: &App) -> Result<Option<SaveJob>, String> {
        let look = self.look(cx)?;
        if self.saved.as_ref() == Some(&(self.choices, look.clone())) {
            return Ok(None);
        }
        Ok(Some(SaveJob {
            owner: self.owner,
            profile_id: self.profile_id.clone(),
            channel: self.channel.clone(),
            mapping: self.mapping,
            base: self.base.clone(),
            choices: self.choices,
            look,
        }))
    }

    /// The application is quitting: writes an edit still waiting out its
    /// pause, to the settings file alone. No window is read or updated (the
    /// chat window can be in the middle of the quit), and the quit is never
    /// held back: an edit that cannot be written (at most the last pause's
    /// worth) is lost. A window without an edit, or holding a value that
    /// cannot be used, writes nothing.
    fn write_on_quit(&mut self, cx: &App) {
        self.autosave = None;
        let Ok(Some(job)) = self.job(cx) else {
            return;
        };
        let Ok(Some(mut settings)) = settings_file::load() else {
            return;
        };
        if settings.profile(&job.profile_id).is_none() {
            return;
        }
        if merge_into_settings(&mut settings, &job, self.utf8_only, &self.i18n).is_ok() {
            let _ = settings_file::save(&settings);
        }
    }

    /// Marks how the channel treats the wallpaper. Inheriting or none
    /// leaves no stale file name behind in the field.
    pub(crate) fn set_wallpaper_choice(&mut self, choice: WallpaperChoice, cx: &mut Context<Self>) {
        self.look.wallpaper_choice = choice;
        if choice != WallpaperChoice::Own {
            self.look
                .wallpaper
                .update(cx, |field, cx| field.set_text("", cx));
        }
        self.error = None;
        cx.notify();
    }

    /// Reset to default: the encodings, the reference mode and the log look
    /// follow the server and the global appearance again, and the sliders and
    /// the fit return to their defaults. The auto-join choice is not one of
    /// them.
    pub(crate) fn reset(&mut self, cx: &mut Context<Self>) {
        self.choices.decode = None;
        self.choices.encode = None;
        self.choices.char_refs = None;
        self.look.clear(cx);
        self.open_list = None;
        self.edited(cx);
    }

    /// Types `text` into the look text field at `index` (the order they are
    /// shown).
    #[cfg(test)]
    pub(crate) fn set_look_field(&mut self, index: usize, text: &str, cx: &mut Context<Self>) {
        self.look.fields()[index].update(cx, |field, cx| field.set_text(text, cx));
    }

    #[cfg(test)]
    pub(crate) fn set_blur(&mut self, blur: u8, cx: &mut Context<Self>) {
        self.look.blur = blur;
        self.look.blur_own = true;
        self.edited(cx);
    }

    /// Escape closes an open list first, then the wallpaper range editor,
    /// then the window.
    pub(crate) fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_list.take().is_some() || self.crop.close() {
            cx.notify();
        } else {
            // A pause not yet over is saved before the window goes.
            if self.autosave.is_some() {
                self.save_now(window, cx);
            }
            window.remove_window();
        }
    }

    fn encoding(&self, side: EncodingSide) -> Option<TextEncoding> {
        match side {
            EncodingSide::Decode => self.choices.decode,
            EncodingSide::Encode => self.choices.encode,
        }
    }

    fn toggle_list(&mut self, side: EncodingSide, cx: &mut Context<Self>) {
        if self.open_list == Some(side) {
            self.open_list = None;
        } else {
            self.open_list = Some(side);
            self.highlight = self
                .encoding(side)
                .and_then(|current| TextEncoding::ALL.iter().position(|e| *e == current))
                .map_or(0, |at| at + 1);
            self.menu_scroll.scroll_to_item(self.highlight);
        }
        cx.notify();
    }

    fn choose(&mut self, side: EncodingSide, option: Option<TextEncoding>, cx: &mut Context<Self>) {
        match side {
            EncodingSide::Decode => self.choices.decode = option,
            EncodingSide::Encode => self.choices.encode = option,
        }
        self.open_list = None;
        self.edited(cx);
    }

    fn set_char_refs(&mut self, mode: Option<CharRefMode>, cx: &mut Context<Self>) {
        self.choices.char_refs = mode;
        self.edited(cx);
    }

    /// The blur and the opacity the sliders show: the effective ones, so an
    /// inherited value is the standard wallpaper's, as the miniature and the
    /// channel log use it. They become the channel's own only when moved.
    pub(crate) fn shown_strengths(&self, cx: &App) -> (u8, u8) {
        let Some(standard) = self.inherited_standard(cx) else {
            return (self.look.blur, self.look.opacity);
        };
        (
            if self.look.blur_own {
                self.look.blur
            } else {
                standard.blur_tenths()
            },
            if self.look.opacity_own {
                self.look.opacity
            } else {
                standard.opacity_percent()
            },
        )
    }

    /// The channel log's standard wallpaper while the channel follows it:
    /// inheriting, with a picture set there.
    fn inherited_standard(&self, cx: &App) -> Option<cayenchat_storage::RegionWallpaper> {
        let inherits = self.look.wallpaper_choice == WallpaperChoice::Inherit
            && self.look.wallpaper.read(cx).text().trim().is_empty();
        if !inherits {
            return None;
        }
        let dark = crate::theme::current(cx).dark;
        self.owner
            .read(cx)
            .ok()
            .map(|chat| chat.appearance.wallpapers(dark).channel_log.clone())
            .filter(|standard| standard.path.is_some())
    }

    /// The fit the buttons mark: the channel's own, else the one it uses.
    pub(crate) fn shown_fit(&self, cx: &App) -> WallpaperFit {
        self.look.fit.unwrap_or_else(|| {
            self.inherited_standard(cx)
                .and_then(|standard| standard.fit)
                .unwrap_or_default()
        })
    }

    pub(crate) fn set_fit(&mut self, fit: WallpaperFit, cx: &mut Context<Self>) {
        self.look.fit = Some(fit);
        self.edited(cx);
    }

    pub(crate) fn set_blur_value(&mut self, blur: u32, cx: &mut Context<Self>) {
        self.look.blur = blur.min(u32::from(MAX_WALLPAPER_BLUR)) as u8;
        self.look.blur_own = true;
        self.edited(cx);
    }

    pub(crate) fn set_opacity(&mut self, opacity: u32, cx: &mut Context<Self>) {
        self.look.opacity = opacity.min(100) as u8;
        self.look.opacity_own = true;
        self.edited(cx);
    }

    fn toggle_enabled(&mut self, cx: &mut Context<Self>) {
        self.choices.enabled = !self.choices.enabled;
        self.edited(cx);
    }

    /// A click on a selector. Enter and Space reach it as keyboard clicks (on
    /// key-up), so they are not handled in `key_down`; with the list open they
    /// choose the highlighted entry, a mouse click closes the list.
    fn click_selector(&mut self, side: EncodingSide, keyboard: bool, cx: &mut Context<Self>) {
        if keyboard && self.open_list == Some(side) {
            let option = self
                .highlight
                .checked_sub(1)
                .map(|at| TextEncoding::ALL[at]);
            self.choose(side, option, cx);
        } else {
            self.toggle_list(side, cx);
        }
    }

    /// Up and Down walk the open list.
    fn move_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.open_list.is_none() {
            return;
        }
        let last = TextEncoding::ALL.len();
        self.highlight = if down {
            (self.highlight + 1).min(last)
        } else {
            self.highlight.saturating_sub(1)
        };
        self.menu_scroll.scroll_to_item(self.highlight);
        cx.notify();
    }

    /// Tab / Shift+Tab: only the dialog's own controls are reachable.
    fn step_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.controls.tab_order(&self.look, self.crop.is_open(), cx);
        let len = order.len();
        let step = if forward { 1 } else { len - 1 };
        let at = order
            .iter()
            .position(|(handle, _)| handle.is_focused(window));
        let (handle, scroll_to) = &order[at.map_or(0, |at| (at + step) % len)];
        // An open list belongs to its selector.
        self.open_list = None;
        window.focus(handle);
        if let Some(child) = *scroll_to {
            self.body_scroll.scroll_to_item(child);
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() || keystroke.is_ime_in_progress() {
            return;
        }
        match keystroke.key.as_str() {
            "escape" => self.escape(window, cx),
            "up" => self.move_highlight(false, cx),
            "down" => self.move_highlight(true, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// One labelled row of the form: the label above its control.
    fn field(label: String, palette: &crate::theme::Theme) -> Div {
        div().flex().flex_col().gap_1().child(
            div()
                .text_color(palette.text)
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
    }

    fn selector(
        &self,
        side: EncodingSide,
        label: String,
        ring: Rgba,
        palette: &crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let (id, control) = match side {
            EncodingSide::Decode => ("channel-decode", &self.controls.decode),
            EncodingSide::Encode => ("channel-encode", &self.controls.encode),
        };
        let inherit = self.i18n.text("channel_settings_inherit");
        let current = self.encoding(side);
        let open = self.open_list == Some(side);
        let mut selector = Self::field(label, palette).child(
            div()
                .id(id)
                .debug_selector(|| id.into())
                .track_focus(control)
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .px_2()
                .py_1()
                .bg(palette.surface)
                .border_1()
                .border_color(if open { ring } else { palette.border })
                .focus(move |style| style.border_color(ring))
                .rounded(px(4.))
                .cursor_pointer()
                .child(
                    div()
                        .min_w_0()
                        .child(current.map_or_else(|| inherit.clone(), |e| e.label().to_owned())),
                )
                .child(div().text_color(palette.text_secondary).child("▾"))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.click_selector(side, event.is_keyboard(), cx)
                })),
        );
        if open {
            let hover = palette.hover;
            let selected = palette.selected;
            let mut menu = div()
                .id("channel-encoding-list")
                .debug_selector(|| "channel-encoding-list".into())
                .track_scroll(&self.menu_scroll)
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                .max_h(MENU_HEIGHT)
                .overflow_y_scroll()
                .bg(palette.surface)
                .border_1()
                .border_color(palette.border)
                .rounded(px(4.));
            let options = std::iter::once(None).chain(TextEncoding::ALL.into_iter().map(Some));
            for (index, option) in options.enumerate() {
                menu = menu.child(
                    div()
                        .id((id, index))
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .hover(move |d| d.bg(hover))
                        .when(current == option, |d| d.bg(selected))
                        .when(self.highlight == index, |d| d.bg(hover))
                        .child(option.map_or(inherit.clone(), |e| e.label().to_owned()))
                        .on_click(cx.listener(move |this, _, _, cx| this.choose(side, option, cx))),
                );
            }
            selector = selector.child(menu);
        }
        selector
    }
}

impl Render for ChannelSettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_pending {
            self.focus_pending = false;
            window.focus(&self.controls.auto_join);
        }
        let palette = settings_theme::palette(cx);
        let ring = settings_theme::current(cx).map_or(palette.text, |native| {
            settings_theme::color(native.defaults.focus_ring_color)
        });
        let heading = self.i18n.format(
            "channel_settings_heading",
            &[("channel", &display::neutralize_bidi(&self.channel))],
        );
        let title = heading.clone();
        let server = self.i18n.format(
            "channel_settings_server",
            &[("server", &display::neutralize_bidi(&self.server))],
        );

        let auto_join = div()
            .id("channel-settings-auto-join")
            .debug_selector(|| "channel-settings-auto-join".into())
            .track_focus(&self.controls.auto_join)
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .rounded(px(4.))
            .border_1()
            .border_color(gpui::transparent_black())
            .focus(move |style| style.border_color(ring))
            .cursor_pointer()
            .child(settings_theme::checkbox(self.choices.enabled, true, cx))
            .child(self.i18n.text("channel_settings_auto_join"))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_enabled(cx)));

        let mut modes = div().flex().flex_wrap().gap_1();
        let inherit = (None, "channel_settings_inherit");
        let choices =
            std::iter::once(inherit).chain(CHAR_REF_CHOICES.map(|(mode, key)| (Some(mode), key)));
        for (index, (mode, key)) in choices.enumerate() {
            let selected = self.choices.char_refs == mode;
            modes = modes.child(
                settings_theme::button(("channel-char-refs", index), selected, cx)
                    .track_focus(&self.controls.char_refs[index])
                    .focus(move |style| style.border_color(ring))
                    .child(self.i18n.text(key))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_char_refs(mode, cx))),
            );
        }
        let char_refs = Self::field(self.i18n.text("channel_settings_char_refs"), &palette)
            .child(modes)
            .child(
                div()
                    .text_sm()
                    .text_color(palette.text_secondary)
                    .child(self.i18n.text("iso2022jp_char_refs_hint")),
            );

        let preview = self.render_wallpaper_preview(window, cx);
        let row = |key: &str, input: &Entity<TextInput>| {
            settings_field(&self.i18n.text(key), input.clone())
        };
        let mut wallpaper_choices = div().flex().flex_wrap().gap_1();
        let shown = match self.look.wallpaper_choice {
            WallpaperChoice::Inherit if !self.look.wallpaper.read(cx).text().trim().is_empty() => {
                WallpaperChoice::Own
            }
            choice => choice,
        };
        for (index, (choice, key)) in WallpaperChoice::ALL.into_iter().enumerate() {
            wallpaper_choices = wallpaper_choices.child(
                settings_theme::button(("channel-look-wallpaper", index), shown == choice, cx)
                    .track_focus(&self.controls.wallpaper_choice[index])
                    .focus(move |style| style.border_color(ring))
                    .child(self.i18n.text(key))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_wallpaper_choice(choice, cx)),
                    ),
            );
        }
        let mut fits = div().flex().flex_wrap().gap_1();
        let shown_fit = self.shown_fit(cx);
        for (index, (fit, key)) in FIT_CHOICES.into_iter().enumerate() {
            fits = fits.child(
                settings_theme::button(("channel-look-fit", index), shown_fit == fit, cx)
                    .track_focus(&self.controls.fit[index])
                    .focus(move |style| style.border_color(ring))
                    .child(self.i18n.text(key))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_fit(fit, cx))),
            );
        }
        let fit = Self::field(self.i18n.text("channel_look_fit"), &palette).child(fits);
        let colors = SliderColors {
            track: palette.border,
            fill: ring,
            knob: palette.surface,
            ring,
        };
        let (shown_blur, shown_opacity) = self.shown_strengths(cx);
        let blur = Self::field(
            self.i18n.format(
                "wallpaper_blur",
                &[("value", &format!("{}.{}", shown_blur / 10, shown_blur % 10))],
            ),
            &palette,
        )
        .child(slider::slider(
            "channel-look-blur",
            u32::from(shown_blur),
            u32::from(MAX_WALLPAPER_BLUR),
            &self.look.blur_track,
            &self.controls.blur,
            colors,
            cx,
            |this, value, _, cx| this.set_blur_value(value, cx),
        ));
        // The same "strength" as the Appearance tab: the share of the picture,
        // the rest of the background color's opacity.
        let strength = 100 - shown_opacity.min(100);
        let opacity = Self::field(
            self.i18n
                .format("wallpaper_strength", &[("value", &strength.to_string())]),
            &palette,
        )
        .child(slider::slider(
            "channel-look-opacity",
            u32::from(strength),
            100,
            &self.look.opacity_track,
            &self.controls.opacity,
            colors,
            cx,
            |this, value, _, cx| this.set_opacity(100 - value.min(100), cx),
        ));
        let look = div()
            .id("channel-look")
            .debug_selector(|| "channel-look".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(palette.text)
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.i18n.text("channel_look_heading")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(palette.text_secondary)
                    .child(self.i18n.text("channel_look_hint")),
            )
            .child(row("channel_look_font_size", &self.look.font_size))
            .child(row("channel_look_background", &self.look.background))
            .child(row("channel_look_alternate", &self.look.alternate))
            .child(row("channel_look_text", &self.look.text))
            .child(
                div()
                    .text_color(palette.text)
                    .child(self.i18n.text("channel_look_wallpaper_choice")),
            )
            .child(wallpaper_choices)
            .child(preview)
            .children(self.render_crop(&palette, ring, cx))
            .child(blur)
            .child(opacity)
            .child(fit);

        let body = div()
            .id("channel-settings-body")
            .debug_selector(|| "channel-settings-body".into())
            .track_scroll(&self.body_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .px_4()
            .py_3()
            .child(
                div()
                    .text_sm()
                    .text_color(palette.text_secondary)
                    .child(self.i18n.text("channel_settings_hint")),
            )
            .child(auto_join)
            .child(self.selector(
                EncodingSide::Decode,
                self.i18n.text("channel_settings_decode"),
                ring,
                &palette,
                cx,
            ))
            .child(self.selector(
                EncodingSide::Encode,
                self.i18n.text("channel_settings_encode"),
                ring,
                &palette,
                cx,
            ))
            .child(char_refs)
            .child(look);

        // Outside the scrolling body, so a rejected save is always visible.
        let error = self.error.clone().map(|error| {
            div()
                .id("channel-settings-error")
                .debug_selector(|| "channel-settings-error".into())
                .px_4()
                .py_2()
                .border_t_1()
                .border_color(palette.border)
                .text_color(palette.warning)
                .child(error)
        });

        let footer = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(palette.border)
            .child(
                settings_theme::button("channel-settings-reset", false, cx)
                    .track_focus(&self.controls.reset)
                    .focus(move |style| style.border_color(ring))
                    .child(self.i18n.text("channel_settings_reset"))
                    .on_click(cx.listener(|this, _, _, cx| this.reset(cx))),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(palette.text_secondary)
                    .child(self.i18n.text("channel_settings_autosave")),
            );

        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .id("channel-settings-title")
                    .text_size(rems(1.1))
                    .font_weight(FontWeight::BOLD)
                    .child(heading),
            )
            .child(
                div()
                    .id("channel-settings-server")
                    .text_sm()
                    .text_color(palette.text_secondary)
                    .child(server),
            );

        let content =
            div()
                .id("channel-settings-window")
                .debug_selector(|| "channel-settings-window".into())
                .key_context("ChannelSettingsDialog")
                .on_action(cx.listener(|this, _: &FocusNextField, window, cx| {
                    this.step_focus(true, window, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusPreviousField, window, cx| {
                    this.step_focus(false, window, cx)
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.key_down(event, window, cx)
                }))
                .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(palette.selected))
                .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                    this.on_wallpaper_drop(paths, cx)
                }))
                .size_full()
                .flex()
                .flex_col()
                .bg(palette.window)
                .text_color(palette.text)
                .text_size(px(13.))
                .when_some(settings_theme::current(cx), |d, native| {
                    d.font_family(native.defaults.font.family.clone())
                        .text_size(px(native.defaults.font.size))
                })
                .child(header)
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(body)
                        .child(scrollbar::scrollbar(
                            "scrollbar-channel-settings",
                            &self.body_scroll,
                            palette.text_muted,
                        )),
                )
                .children(error)
                .child(footer);
        decorations::window_frame(window, cx, title, content)
    }
}

impl ChatWindow {
    /// Opens the settings window of `channel` of the server `profile_id`, or
    /// raises it when it is open already. The values are those of the
    /// settings file, which a settings window or `/charset` may have updated
    /// since this window last saved. The window saves by itself, so this
    /// window shows each change while it is adjusted.
    pub(crate) fn open_channel_settings_window(
        &mut self,
        profile_id: &str,
        channel: &str,
        mapping: CaseMapping,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A transfer writes the whole file; an edit saved meanwhile would be
        // overwritten or would overwrite its result.
        if self.transfer_dialog_busy() {
            return;
        }
        let same = |left: &str, right: &str| mapping.same(left, right);
        self.channel_settings_windows
            .retain(|open| open.handle.update(cx, |_, _, _| ()).is_ok());
        if let Some(open) = self
            .channel_settings_windows
            .iter()
            .find(|open| open.profile_id == profile_id && same(&open.channel, channel))
        {
            let _ = open
                .handle
                .update(cx, |_, window, _| window.activate_window());
            return;
        }
        let latest = match settings_file::load() {
            Ok(Some(settings)) if settings.profile(profile_id).is_some() => Some(settings),
            _ => None,
        };
        let values = latest.as_ref().unwrap_or(&self.saved);
        let Some(profile) = values.profile(profile_id) else {
            return;
        };
        let choices = Choices::of(profile, channel, mapping);
        let look = values
            .appearance
            .channel_look(profile_id, channel, same)
            .cloned();
        let server = display::neutralize_bidi(if profile.display_name.is_empty() {
            &profile.host
        } else {
            &profile.display_name
        })
        .into_owned();
        let title = self.i18n.format(
            "channel_settings_heading",
            &[("channel", &display::neutralize_bidi(channel))],
        );
        settings_theme::refresh(cx);
        let owner = window.window_handle().downcast::<ChatWindow>().unwrap();
        let (profile_owned, channel_owned) = (profile_id.to_owned(), channel.to_owned());
        let utf8_only = self.channel_settings_context(profile_id).1;
        let i18n = self.i18n.clone();
        let min_size = size(px(420.), px(360.));
        let wanted = crate::about::frame_size(size(px(520.), px(680.)));
        let (bounds, display_id) = self.secondary_window_bounds(window, wanted, min_size, cx);
        #[cfg(target_os = "windows")]
        let owner_scale = window.scale_factor();
        let opened = cx.open_window(
            WindowOptions {
                app_id: Some(crate::APP_ID.into()),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                display_id,
                window_min_size: Some(min_size),
                titlebar: Some(TitlebarOptions {
                    title: Some("CayenChat".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                // The channel name is server text: it reaches the OS title
                // only through this gate.
                window.os_set_title(&title);
                cx.new(|cx| {
                    ChannelSettingsView::new(
                        owner,
                        profile_owned,
                        &channel_owned,
                        mapping,
                        server,
                        utf8_only,
                        choices,
                        look.as_ref(),
                        i18n,
                        window,
                        cx,
                    )
                })
            },
        );
        match opened {
            Ok(handle) => {
                #[cfg(target_os = "windows")]
                if !cfg!(test) {
                    let _ = handle.update(cx, |_, new_window, _| {
                        if display_id.is_some() {
                            crate::placement::place_native(new_window, bounds, owner_scale);
                        }
                    });
                }
                #[cfg(target_os = "windows")]
                if self.always_on_top {
                    let _ = handle.update(cx, |_, new_window, _| {
                        if let Err(error) = crate::set_window_always_on_top(new_window, true) {
                            log::warn!("Could not keep the channel window on top: {error}");
                        }
                    });
                }
                self.channel_settings_windows.push(OpenChannelSettings {
                    profile_id: profile_id.to_owned(),
                    channel: channel.to_owned(),
                    handle,
                });
            }
            Err(error) => {
                let message = self
                    .i18n
                    .format("settings_open_failed", &[("error", &error.to_string())]);
                self.report(Some(message), cx);
            }
        }
        cx.notify();
    }

    /// Shows `look` (changed from `base`) in the log of the channel before
    /// it is saved; nothing is written.
    pub(crate) fn preview_channel_look(
        &mut self,
        profile_id: &str,
        channel: &str,
        mapping: CaseMapping,
        base: &ChannelLook,
        look: &ChannelLook,
        cx: &mut Context<Self>,
    ) {
        let same = |a: &str, b: &str| mapping.same(a, b);
        // Always from the saved values, never from an earlier preview: a field
        // edited back to its base value is then the saved one again.
        let mut merged = merge_look(
            self.saved
                .appearance
                .channel_look(profile_id, channel, same),
            base,
            look,
        );
        merged.server = profile_id.to_owned();
        merged.channel = channel.to_owned();
        let mut appearance = self.saved.appearance.clone();
        if appearance.set_channel_look(merged, same).is_ok() {
            self.apply_appearance(appearance, self.theme_mode, cx);
        }
    }

    /// Takes a preview back: the appearance is the saved one again.
    pub(crate) fn discard_look_preview(&mut self, cx: &mut Context<Self>) {
        let appearance = self.saved.appearance.clone();
        self.apply_appearance(appearance, self.theme_mode, cx);
    }

    /// Writes the channel's settings into the settings file, starting from
    /// the file, and applies them as the settings window's save does; nothing
    /// is joined or parted. The reason is returned when it cannot be done.
    #[allow(clippy::too_many_arguments)] // The window's values and where they go.
    pub(crate) fn apply_channel_settings(
        &mut self,
        profile_id: &str,
        channel: &str,
        mapping: CaseMapping,
        base: &(Choices, ChannelLook),
        choices: Choices,
        look: ChannelLook,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.transfer_dialog_busy() {
            return Err(self.i18n.text("channel_settings_busy"));
        }
        let mut settings = match settings_file::load()? {
            Some(settings) if settings.profile(profile_id).is_some() => settings,
            _ => self.saved.clone(),
        };
        let utf8_only = self.channel_settings_context(profile_id).1;
        let job = SaveJob {
            owner: window.window_handle().downcast::<ChatWindow>().unwrap(),
            profile_id: profile_id.to_owned(),
            channel: channel.to_owned(),
            mapping,
            base: base.clone(),
            choices,
            look,
        };
        merge_into_settings(&mut settings, &job, utf8_only, &self.i18n)?;
        settings_file::save(&settings)?;
        let previous = self.saved.clone();
        self.apply_settings_change(&previous, settings, true, window, cx);
        // An open settings window takes the file's values over itself.
        Ok(())
    }
}

/// An edit of the channel settings window, ready to be written: the values
/// it holds and the state they were changed from.
pub(crate) struct SaveJob {
    owner: WindowHandle<ChatWindow>,
    profile_id: String,
    channel: String,
    mapping: CaseMapping,
    base: (Choices, ChannelLook),
    choices: Choices,
    look: ChannelLook,
}

impl SaveJob {
    /// Writes it through the chat window, which shows it at once.
    fn run(&self, cx: &mut App) -> gpui::Result<Result<(), String>> {
        self.owner.update(cx, |chat, window, cx| {
            chat.apply_channel_settings(
                &self.profile_id,
                &self.channel,
                self.mapping,
                &self.base,
                self.choices,
                self.look.clone(),
                window,
                cx,
            )
        })
    }
}

/// Puts the job's edit into `settings`: only what the window changed from
/// its base, the rest being the latest values of `settings`.
fn merge_into_settings(
    settings: &mut Settings,
    job: &SaveJob,
    utf8_only: bool,
    i18n: &Localizer,
) -> Result<(), String> {
    let (profile_id, channel, mapping) = (&job.profile_id, &job.channel, job.mapping);
    let latest = settings
        .profile(profile_id)
        .map_or(job.base.0, |profile| Choices::of(profile, channel, mapping));
    let choices = merge_choices(latest, job.base.0, job.choices);
    let latest_look = settings
        .appearance
        .channel_look(profile_id, channel, |a, b| mapping.same(a, b));
    let look = merge_look(latest_look, &job.base.1, &job.look);
    save_channel_choices(
        settings, profile_id, channel, mapping, choices, look, utf8_only, i18n,
    )
    .map(|_| ())
}

/// A channel settings window that is open, so a second request raises it.
pub(crate) struct OpenChannelSettings {
    profile_id: String,
    channel: String,
    pub(crate) handle: WindowHandle<ChannelSettingsView>,
}
