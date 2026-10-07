use std::default::Default;

use x11rb::protocol::{Event, xproto};
use xim::{AHashMap, AttributeName, Client, ClientError, ClientHandler, InputStyle};

pub enum XimCallbackEvent {
    XimXEvent(x11rb::protocol::Event),
    XimPreeditEvent(xproto::Window, String),
    XimCommitEvent(xproto::Window, String),
}

/// Picks the input style to create the input context with from the raw
/// `XNQueryInputStyle` reply (u16 count, u16 padding, then u32 styles).
///
/// The style is fixed at creation time, so it must be one the server offered:
/// preedit callbacks with no status area, falling back to the previous
/// hard-coded value when the reply is unusable.
pub(crate) fn choose_input_style(raw: Option<&[u8]>) -> InputStyle {
    let offered = raw
        .filter(|raw| raw.len() >= 4)
        .map(|raw| {
            raw[4..]
                .chunks_exact(4)
                .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    [
        InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NOTHING,
        InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NONE,
    ]
    .into_iter()
    .find(|style| offered.contains(&style.bits()))
    .unwrap_or(InputStyle::PREEDIT_CALLBACKS)
}

pub struct XimHandler {
    pub im_id: u16,
    pub ic_id: u16,
    pub input_style: InputStyle,
    pub connected: bool,
    pub window: xproto::Window,
    pub last_callback_event: Option<XimCallbackEvent>,
}

impl XimHandler {
    pub fn new() -> Self {
        Self {
            im_id: Default::default(),
            ic_id: Default::default(),
            input_style: InputStyle::PREEDIT_CALLBACKS,
            connected: false,
            window: Default::default(),
            last_callback_event: None,
        }
    }
}

impl<C: Client<XEvent = xproto::KeyPressEvent>> ClientHandler<C> for XimHandler {
    fn handle_connect(&mut self, client: &mut C) -> Result<(), ClientError> {
        client.open("C")
    }

    fn handle_open(&mut self, client: &mut C, input_method_id: u16) -> Result<(), ClientError> {
        self.im_id = input_method_id;

        client.get_im_values(input_method_id, &[AttributeName::QueryInputStyle])
    }

    fn handle_get_im_values(
        &mut self,
        client: &mut C,
        input_method_id: u16,
        attributes: AHashMap<AttributeName, Vec<u8>>,
    ) -> Result<(), ClientError> {
        self.input_style = choose_input_style(
            attributes
                .get(&AttributeName::QueryInputStyle)
                .map(Vec::as_slice),
        );
        let ic_attributes = client
            .build_ic_attributes()
            .push(AttributeName::InputStyle, self.input_style)
            .push(AttributeName::ClientWindow, self.window)
            .push(AttributeName::FocusWindow, self.window)
            .build();
        client.create_ic(input_method_id, ic_attributes)
    }

    fn handle_create_ic(
        &mut self,
        _client: &mut C,
        _input_method_id: u16,
        input_context_id: u16,
    ) -> Result<(), ClientError> {
        self.connected = true;
        self.ic_id = input_context_id;
        Ok(())
    }

    fn handle_commit(
        &mut self,
        _client: &mut C,
        _input_method_id: u16,
        _input_context_id: u16,
        text: &str,
    ) -> Result<(), ClientError> {
        self.last_callback_event = Some(XimCallbackEvent::XimCommitEvent(
            self.window,
            String::from(text),
        ));
        Ok(())
    }

    fn handle_forward_event(
        &mut self,
        _client: &mut C,
        _input_method_id: u16,
        _input_context_id: u16,
        _flag: xim::ForwardEventFlag,
        xev: C::XEvent,
    ) -> Result<(), ClientError> {
        match xev.response_type {
            x11rb::protocol::xproto::KEY_PRESS_EVENT => {
                self.last_callback_event = Some(XimCallbackEvent::XimXEvent(Event::KeyPress(xev)));
            }
            x11rb::protocol::xproto::KEY_RELEASE_EVENT => {
                self.last_callback_event =
                    Some(XimCallbackEvent::XimXEvent(Event::KeyRelease(xev)));
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_close(&mut self, client: &mut C, _input_method_id: u16) -> Result<(), ClientError> {
        client.disconnect()
    }

    fn handle_preedit_draw(
        &mut self,
        _client: &mut C,
        _input_method_id: u16,
        _input_context_id: u16,
        _caret: i32,
        _chg_first: i32,
        _chg_len: i32,
        _status: xim::PreeditDrawStatus,
        preedit_string: &str,
        _feedbacks: Vec<xim::Feedback>,
    ) -> Result<(), ClientError> {
        // XIMReverse: 1, XIMPrimary: 8, XIMTertiary: 32: selected text
        // XIMUnderline: 2, XIMSecondary: 16: underlined text
        // XIMHighlight: 4: normal text
        // XIMVisibleToForward: 64, XIMVisibleToBackward: 128, XIMVisibleCenter: 256: text align position
        // XIMPrimary, XIMHighlight, XIMSecondary, XIMTertiary are not specified,
        // but interchangeable as above
        // Currently there's no way to support these.
        self.last_callback_event = Some(XimCallbackEvent::XimPreeditEvent(
            self.window,
            String::from(preedit_string),
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(styles: &[u32]) -> Vec<u8> {
        let mut raw = (styles.len() as u16).to_ne_bytes().to_vec();
        raw.extend([0, 0]);
        for style in styles {
            raw.extend(style.to_ne_bytes());
        }
        raw
    }

    #[test]
    fn picks_style_offered_by_ibus() {
        let raw = reply(&[0x0404, 0x0402, 0x0408, 0x0204, 0x0202, 0x0208]);
        let style = choose_input_style(Some(&raw));
        assert_eq!(style.bits(), 0x0402);
    }

    #[test]
    fn falls_back_without_usable_reply() {
        assert_eq!(choose_input_style(None), InputStyle::PREEDIT_CALLBACKS);
        assert_eq!(
            choose_input_style(Some(&reply(&[0x0404]))),
            InputStyle::PREEDIT_CALLBACKS
        );
        assert_eq!(choose_input_style(Some(&[1])), InputStyle::PREEDIT_CALLBACKS);
    }
}
