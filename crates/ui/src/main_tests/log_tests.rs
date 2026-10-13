// SPDX-License-Identifier: GPL-3.0-only
use super::{LogPosition, LogSelection, channel_activity_text};
use cayenchat_irc_core::ChannelActivityKind;
use cayenchat_model::ConversationId;

#[test]
fn channel_activity_uses_english_phrases_with_optional_details() {
    assert_eq!(
        channel_activity_text(
            "kaeru",
            ChannelActivityKind::Joined {
                mask: Some("~kaeru@host".into()),
            },
        ),
        "kaeru has joined (~kaeru@host)"
    );
    assert_eq!(
        channel_activity_text("kaeru", ChannelActivityKind::Left { reason: None }),
        "kaeru has left"
    );
    assert_eq!(
        channel_activity_text(
            "kaeru",
            ChannelActivityKind::Quit {
                reason: Some("bye".into()),
            },
        ),
        "kaeru has quit (bye)"
    );
    assert_eq!(
        channel_activity_text(
            "kaeru",
            ChannelActivityKind::NickChanged { to: "tepeu".into() },
        ),
        "kaeru is now known as tepeu"
    );
    assert_eq!(
        channel_activity_text(
            "tepeu",
            ChannelActivityKind::ModeChanged {
                modes: "+o kaeru".into(),
            },
        ),
        "tepeu has changed mode: +o kaeru"
    );
    assert_eq!(
        channel_activity_text(
            "op",
            ChannelActivityKind::Kicked {
                target: "kaeru".into(),
                reason: Some("spam".into()),
                own: false,
            },
        ),
        "kaeru was kicked by op (spam)"
    );
    assert_eq!(
        channel_activity_text(
            "op",
            ChannelActivityKind::Kicked {
                target: "me".into(),
                reason: None,
                own: true,
            },
        ),
        "You were kicked from the channel by op"
    );
    assert_eq!(
        channel_activity_text(
            "server",
            ChannelActivityKind::JoinFailed {
                banned: true,
                reason: "Cannot join channel (+b)".into(),
            },
        ),
        "Cannot join: you are banned from this channel (Cannot join channel (+b))"
    );
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

#[test]
fn link_colors_apply_to_log_urls_only_and_default_to_the_old_look() {
    use crate::theme::Theme;
    use cayenchat_storage::{Appearance, DarkColors, ThemeMode};
    use gpui::{WindowAppearance, rgb};

    let light =
        |appearance: &Appearance| Theme::new(ThemeMode::Light, WindowAppearance::Light, appearance);
    let default = light(&Appearance::default());
    assert_eq!(default.panes.log_link, rgb(0x0645ad));
    assert_eq!(default.panes.log_link, default.link);
    assert_eq!(super::link_background(&default, true, false), None);
    let tint = super::link_background(&default, true, true).unwrap();
    assert_eq!(
        (tint.a, tint.r, tint.g, tint.b),
        (
            0.14,
            0x06 as f32 / 255.,
            0x45 as f32 / 255.,
            0xad as f32 / 255.
        )
    );
    assert_eq!(super::link_background(&default, false, false), None);

    let dark = Theme::new(
        ThemeMode::Dark,
        WindowAppearance::Dark,
        &Appearance::default(),
    );
    assert_eq!(dark.panes.log_link, rgb(0x78aaff));

    let custom = Appearance {
        link_color: "#AA0000".into(),
        link_background: "#FFFF00".into(),
        dark: DarkColors {
            link_color: "#00FF00".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let theme = light(&custom);
    assert_eq!(theme.panes.log_link, rgb(0xaa0000));
    // The rest of the UI keeps its link color.
    assert_eq!(theme.link, default.link);
    assert_eq!(
        super::link_background(&theme, true, false),
        Some(rgb(0xffff00))
    );
    assert_eq!(
        super::link_background(&theme, true, true),
        Some(rgb(0xffff00))
    );
    assert_eq!(super::link_background(&theme, false, false), None);
    // The dark theme reads its own values and stays automatic.
    let theme = Theme::new(ThemeMode::Dark, WindowAppearance::Dark, &custom);
    assert_eq!(theme.panes.log_link, rgb(0x00ff00));
    assert_eq!(theme.panes.log_link_background, None);
}
