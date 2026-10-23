// SPDX-License-Identifier: GPL-3.0-only
//! The gate in front of the dialogs and window titles the operating system
//! draws. Every `Window::prompt` and `Window::set_window_title` call goes
//! through here, so text from a server, a peer or a value built by the
//! caller is cleaned right before it leaves (`model::display::os_title`,
//! `os_body`), whoever called. Desktop notifications have their gate in
//! `Notifier::show`.

use cayenchat_model::display::{os_body, os_title};
use gpui::{App, PromptButton, PromptLevel, Window};

pub trait WindowOsText {
    /// `Window::prompt` with the title, detail and button labels cleaned.
    fn os_prompt(
        &mut self,
        level: PromptLevel,
        title: &str,
        detail: Option<&str>,
        buttons: &[PromptButton],
        cx: &mut App,
    ) -> futures_channel::oneshot::Receiver<usize>;

    /// `Window::set_window_title` with the title cleaned.
    fn os_set_title(&mut self, title: &str);
}

impl WindowOsText for Window {
    fn os_prompt(
        &mut self,
        level: PromptLevel,
        title: &str,
        detail: Option<&str>,
        buttons: &[PromptButton],
        cx: &mut App,
    ) -> futures_channel::oneshot::Receiver<usize> {
        let buttons: Vec<PromptButton> = buttons.iter().map(clean_button).collect();
        #[allow(clippy::disallowed_methods)] // the gate
        self.prompt(
            level,
            &os_title(title),
            detail.map(os_body).as_deref(),
            &buttons,
            cx,
        )
    }

    fn os_set_title(&mut self, title: &str) {
        #[allow(clippy::disallowed_methods)] // the gate
        self.set_window_title(&os_title(title));
    }
}

fn clean_button(button: &PromptButton) -> PromptButton {
    let label = os_title(button.label());
    match button {
        PromptButton::Ok(_) => PromptButton::ok(label),
        PromptButton::Cancel(_) => PromptButton::cancel(label),
        PromptButton::Other(_) => PromptButton::new(label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_labels_are_cleaned_and_keep_their_role() {
        let dirty = "\u{2}O\u{202E}K\n";
        assert_eq!(
            clean_button(&PromptButton::ok(dirty)),
            PromptButton::ok("O\u{200B}K ")
        );
        assert_eq!(
            clean_button(&PromptButton::cancel(dirty)),
            PromptButton::cancel("O\u{200B}K ")
        );
        assert_eq!(
            clean_button(&PromptButton::new(dirty)),
            PromptButton::new("O\u{200B}K ")
        );
    }

    /// Nothing may reach the operating system around the gate.
    #[test]
    fn no_source_file_calls_the_raw_window_methods() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![dir];
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "os_text.rs")
            {
                let source = std::fs::read_to_string(&path).unwrap();
                for call in ["window.prompt(", ".set_window_title("] {
                    assert!(!source.contains(call), "{} calls {call}", path.display());
                }
            }
        }
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use gpui::TestAppContext;

    const DIRTY: &str = "a\u{202E}b\u{3}c\u{2}d\u{2028}e\u{2029}f\u{7}g\nh";

    fn assert_clean(text: &str, newline_ok: bool) {
        assert!(
            !text.chars().any(|c| matches!(c,
                '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}')
                || (c.is_control() && !(newline_ok && c == '\n'))),
            "{text:?}"
        );
    }

    #[gpui::test]
    fn prompt_text_is_cleaned_whoever_calls(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let long = "x".repeat(10_000);
        cx.update(|window, cx| {
            drop(window.os_prompt(
                PromptLevel::Info,
                &format!("{DIRTY}{long}"),
                Some(&format!("{DIRTY}{long}")),
                &[PromptButton::ok("OK")],
                cx,
            ));
        });
        let (title, detail) = cx.pending_prompt().expect("prompt");
        assert_clean(&title, false);
        assert_clean(&detail, true);
        assert_eq!(title, os_title(&format!("{DIRTY}{long}")));
        assert_eq!(detail, os_body(&format!("{DIRTY}{long}")));
        assert!(detail.contains('\n') && title.chars().count() <= 200);
        assert!(detail.chars().count() <= 2000);
    }

    #[gpui::test]
    fn window_title_is_cleaned_whoever_calls(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let dirty = format!("{DIRTY}{}", "y".repeat(10_000));
        cx.update(|window, _| window.os_set_title(&dirty));
        let title = cx.window_title().expect("title");
        assert_clean(&title, false);
        assert_eq!(title, os_title(&dirty));
        assert!(title.chars().count() <= 200);
    }
}
