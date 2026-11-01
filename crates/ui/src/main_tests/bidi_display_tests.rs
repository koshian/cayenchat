// SPDX-License-Identifier: GPL-3.0-only
use super::{ChannelDragPreview, ChatWindow, MemberPromptKind, member_prompt_title};
use crate::localization::Localizer;
use cayenchat_storage::Language;

const RAW: &str = "mal\u{202E}lory";
const SAFE: &str = "mal\u{200B}lory";

#[test]
fn dialog_headings_neutralize_the_nickname() {
    let i18n = Localizer::new(Language::English);
    for kind in [
        MemberPromptKind::PrivateMessage,
        MemberPromptKind::Invite,
        MemberPromptKind::Join,
        MemberPromptKind::Nick,
    ] {
        let title = member_prompt_title(&i18n, kind, RAW);
        assert!(!title.contains('\u{202E}'), "{title}");
    }
    let title = member_prompt_title(&i18n, MemberPromptKind::PrivateMessage, RAW);
    assert!(title.contains(SAFE), "{title}");
}

#[test]
fn not_found_and_rejected_headings_neutralize_the_nickname() {
    let i18n = Localizer::new(Language::English);
    for key in ["whois_not_found", "nick_prompt_title"] {
        let text = i18n.format_nickname(key, RAW);
        assert!(text.contains(SAFE), "{text}");
        assert!(!text.contains('\u{202E}'), "{text}");
    }
}

#[test]
fn disconnected_status_reason_neutralizes_bidi_controls() {
    let i18n = Localizer::new(Language::English);
    let text = ChatWindow::disconnected_status_text(&i18n, "mal\u{202E}lory");
    assert!(text.contains("mal\u{200B}lory"), "{text}");
    assert!(!text.contains('\u{202E}'), "{text}");
}

#[test]
fn drag_preview_neutralizes_bidi_controls_in_channel_names() {
    let preview = ChannelDragPreview::new("#invoice\u{202E}fdp.exe");
    assert_eq!(preview.0, "#invoice\u{200B}fdp.exe");
}
