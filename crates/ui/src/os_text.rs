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
        self.prompt(
            level,
            &os_title(title),
            detail.map(os_body).as_deref(),
            &buttons,
            cx,
        )
    }

    fn os_set_title(&mut self, title: &str) {
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
