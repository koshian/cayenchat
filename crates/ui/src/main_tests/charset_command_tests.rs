// SPDX-License-Identifier: GPL-3.0-only
use super::{ChatWindow, Worker};
use crate::charset_command::parse;
use crate::settings_file::{self, TestFile};
use cayenchat_irc_core::Event;
use cayenchat_model::NetworkId;
use cayenchat_storage::TextEncoding;
use gpui::{TestAppContext, VisualTestContext};

pub(super) fn open_chat<'a>(
    cx: &'a mut TestAppContext,
    channels: &str,
) -> (
    gpui::Entity<ChatWindow>,
    &'a mut VisualTestContext,
    TestFile,
) {
    crate::support::install_test_globals(cx);
    let mut settings = crate::settings_with_channels(channels);
    settings.servers[0].encoding = TextEncoding::EucJp;
    let file = TestFile::with(&settings);
    let (chat, cx) = cx.add_window_view(|window, cx| {
        ChatWindow::with_settings(settings.clone(), None, window, cx)
    });
    chat.update(cx, |chat, cx| {
        let joined = channels
            .split(',')
            .map(|channel| Event::Joined {
                channel: channel.trim().to_owned(),
            })
            .collect::<Vec<_>>();
        let mut events = vec![Event::Registered {
            nickname: "me".into(),
        }];
        events.extend(joined);
        chat.handle_events(NetworkId(1), events, Worker::Running, cx);
        let channel = chat.state.conversations()[0].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(channel));
    });
    (chat, cx, file)
}

pub(super) fn run(
    chat: &gpui::Entity<ChatWindow>,
    cx: &mut VisualTestContext,
    line: &str,
) -> Result<String, String> {
    chat.update(cx, |chat, cx| {
        let request = parse(line)
            .unwrap()
            .map_err(|problem| chat.charset_problem(&problem))?;
        chat.run_charset(request, cx)?;
        let channel = chat.state.selected_channel().unwrap();
        Ok(channel.messages.last().unwrap().text.clone())
    })
}

fn stored(
    chat: &gpui::Entity<ChatWindow>,
    cx: &mut VisualTestContext,
) -> Vec<cayenchat_storage::ChannelCharset> {
    chat.read_with(cx, |chat, _| chat.saved.servers[0].channel_charsets.clone())
}

#[gpui::test]
fn the_server_encoding_is_inherited_until_a_side_is_set(cx: &mut TestAppContext) {
    let (chat, cx, file) = open_chat(cx, "#a");
    let shown = run(&chat, cx, "/charset").unwrap();
    assert!(shown.contains("#a"), "{shown}");
    assert_eq!(shown.matches("EUC-JP").count(), 2, "{shown}");
    assert!(stored(&chat, cx).is_empty());

    // One side: the other keeps following the server.
    run(&chat, cx, "/charset decode iso-2022-jp").unwrap();
    let entries = stored(&chat, cx);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].decode, Some(TextEncoding::Iso2022Jp));
    assert_eq!(entries[0].encode, None);
    let shown = run(&chat, cx, "/charset").unwrap();
    assert!(shown.contains("ISO-2022-JP"), "{shown}");
    assert!(shown.contains("EUC-JP"), "{shown}");

    // The other side is added without losing the first.
    run(&chat, cx, "/charset encode CP949").unwrap();
    let entries = stored(&chat, cx);
    assert_eq!(entries[0].decode, Some(TextEncoding::Iso2022Jp));
    assert_eq!(entries[0].encode, Some(TextEncoding::EucKr));

    // Both sides at once, then back to the server's.
    run(&chat, cx, "/charset KOI8-R").unwrap();
    assert_eq!(stored(&chat, cx)[0].decode, Some(TextEncoding::Koi8R));
    assert_eq!(stored(&chat, cx)[0].encode, Some(TextEncoding::Koi8R));
    run(&chat, cx, "/charset reset").unwrap();
    assert!(stored(&chat, cx).is_empty());
    let nothing = run(&chat, cx, "/charset reset").unwrap();
    assert!(nothing.contains("#a"), "{nothing}");

    // What was saved is what a later start reads, and reconnects use it.
    chat.update(cx, |chat, _| {
        let config =
            cayenchat_irc_core::ConnectionConfig::tls("example.net".into(), "me".into(), vec![]);
        chat.sessions.get_mut(&NetworkId(1)).unwrap().active_config = Some(config);
    });
    run(&chat, cx, "/charset Shift_JIS").unwrap();
    let file_settings = settings_file::load().unwrap().unwrap();
    assert_eq!(file_settings.servers[0].channel_charsets, stored(&chat, cx));
    chat.read_with(cx, |chat, _| {
        let config = chat.sessions[&NetworkId(1)].active_config.as_ref().unwrap();
        assert_eq!(config.channel_encodings.len(), 1);
        assert_eq!(config.channel_encodings[0].channel, "#a");
        assert_eq!(
            config.channel_encodings[0].encode.as_deref(),
            Some("Shift_JIS")
        );
        assert!(config.validate().is_ok());
    });
    drop(file);
}

#[gpui::test]
fn bad_input_and_other_tabs_change_nothing(cx: &mut TestAppContext) {
    let (chat, cx, file) = open_chat(cx, "#a");
    let error = run(&chat, cx, "/charset klingon").unwrap_err();
    assert!(
        error.contains("klingon") && error.contains("UTF-8"),
        "{error}"
    );
    assert!(run(&chat, cx, "/charset ISO-8859-1").is_err());
    assert!(run(&chat, cx, "/charset decode").is_err());
    assert!(stored(&chat, cx).is_empty());

    // A failed save leaves the setting as it was.
    settings_file::fail_saves(true);
    assert!(run(&chat, cx, "/charset Big5").is_err());
    settings_file::fail_saves(false);
    assert!(stored(&chat, cx).is_empty());

    // The server log has no channel to apply it to.
    chat.update(cx, |chat, _| {
        chat.state
            .dispatch(cayenchat_app::Command::SelectServer(NetworkId(1)));
    });
    let error = run(&chat, cx, "/charset Big5").unwrap_err();
    assert!(error.contains("/charset"), "{error}");
    assert!(stored(&chat, cx).is_empty());
    drop(file);
}

#[gpui::test]
fn each_channel_keeps_its_own(cx: &mut TestAppContext) {
    let (chat, cx, file) = open_chat(cx, "#a,#b");
    run(&chat, cx, "/charset Big5").unwrap();
    chat.update(cx, |chat, _| {
        let second = chat.state.conversations()[1].id;
        chat.state
            .dispatch(cayenchat_app::Command::SelectChannel(second));
    });
    let shown = run(&chat, cx, "/charset").unwrap();
    assert!(!shown.contains("Big5"), "{shown}");
    run(&chat, cx, "/charset GBK").unwrap();
    let entries = stored(&chat, cx);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].channel, "#a");
    assert_eq!(entries[0].decode, Some(TextEncoding::Big5));
    assert_eq!(entries[1].channel, "#b");
    assert_eq!(entries[1].decode, Some(TextEncoding::Gbk));
    drop(file);
}
