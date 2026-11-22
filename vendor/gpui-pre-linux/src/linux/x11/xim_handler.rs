use std::default::Default;

use x11rb::protocol::{Event, xproto};
use xim::{AHashMap, Attribute, AttributeName, Client, ClientError, ClientHandler, InputStyle};

pub enum XimCallbackEvent {
    XimXEvent(x11rb::protocol::Event),
    XimPreeditEvent(xproto::Window, String),
    XimCommitEvent(xproto::Window, String),
}

pub struct XimHandler {
    pub im_id: u16,
    pub ic_id: u16,
    /// The style the input context is created with; chosen from the server's
    /// `XNQueryInputStyle` reply and kept for re-creating the context.
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

    /// Attributes for `create_ic`, optionally with the preedit spot.
    pub fn create_ic_attributes<C: Client>(
        &self,
        client: &C,
        spot: Option<xim::Point>,
    ) -> Vec<Attribute> {
        let mut builder = client
            .build_ic_attributes()
            .push(AttributeName::InputStyle, self.input_style)
            .push(AttributeName::ClientWindow, self.window)
            .push(AttributeName::FocusWindow, self.window);
        if let Some(spot) = spot {
            builder = builder.nested_list(AttributeName::PreeditAttributes, |b| {
                b.push(AttributeName::SpotLocation, spot);
            });
        }
        builder.build()
    }

    /// Attributes for `set_ic_values`. `InputStyle` and `ClientWindow` are
    /// creation-time only, so servers reject or ignore them here (issue #227).
    pub fn spot_ic_attributes<C: Client>(&self, client: &C, spot: xim::Point) -> Vec<Attribute> {
        client
            .build_ic_attributes()
            .push(AttributeName::FocusWindow, self.window)
            .nested_list(AttributeName::PreeditAttributes, |b| {
                b.push(AttributeName::SpotLocation, spot);
            })
            .build()
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
        self.input_style = xim::choose_input_style(
            attributes
                .get(&AttributeName::QueryInputStyle)
                .map(Vec::as_slice),
        );
        let ic_attributes = self.create_ic_attributes(client, None);
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
    use xim::ClientCore;

    /// A client that only knows the input context attribute ids; the tests
    /// look at which attributes the handler asks for.
    struct IdsOnly(AHashMap<AttributeName, u16>);

    impl ClientCore for IdsOnly {
        type XEvent = xproto::KeyPressEvent;

        fn set_attrs(&mut self, _ic_attrs: Vec<xim::Attr>, _im_attrs: Vec<xim::Attr>) {}
        fn ic_attributes(&self) -> &AHashMap<AttributeName, u16> {
            &self.0
        }
        fn im_attributes(&self) -> &AHashMap<AttributeName, u16> {
            &self.0
        }
        fn serialize_event(&self, _xev: &Self::XEvent) -> xim::XEvent {
            unreachable!()
        }
        fn deserialize_event(&self, _xev: &xim::XEvent) -> Self::XEvent {
            unreachable!()
        }
        fn send_req(&mut self, _req: xim::Request) -> Result<(), ClientError> {
            Ok(())
        }
    }

    const NAMES: [AttributeName; 5] = [
        AttributeName::InputStyle,
        AttributeName::ClientWindow,
        AttributeName::FocusWindow,
        AttributeName::PreeditAttributes,
        AttributeName::SpotLocation,
    ];

    fn id(name: AttributeName) -> u16 {
        NAMES.iter().position(|n| *n == name).unwrap() as u16 + 1
    }

    fn client() -> IdsOnly {
        IdsOnly(NAMES.iter().map(|n| (*n, id(*n))).collect())
    }

    fn ids(attributes: &[Attribute]) -> Vec<u16> {
        attributes.iter().map(|a| a.id).collect()
    }

    fn style_of(attributes: &[Attribute]) -> u32 {
        let value = &attributes
            .iter()
            .find(|a| a.id == id(AttributeName::InputStyle))
            .expect("input style attribute")
            .value;
        u32::from_ne_bytes(value[..4].try_into().unwrap())
    }

    #[test]
    fn creation_uses_the_chosen_style_with_and_without_a_spot() {
        let mut handler = XimHandler::new();
        // What IBus offers: the hard-coded PREEDIT_CALLBACKS alone (0x2) is not among them.
        handler.input_style = xim::choose_input_style(Some(&[
            6, 0, 0, 0, 0x04, 0x04, 0, 0, 0x02, 0x04, 0, 0, 0x08, 0x04, 0, 0, 0x04, 0x02, 0, 0,
            0x02, 0x02, 0, 0, 0x08, 0x02, 0, 0,
        ]));
        assert_eq!(handler.input_style.bits(), 0x0402);

        let without = handler.create_ic_attributes(&client(), None);
        assert_eq!(style_of(&without), 0x0402);
        assert_eq!(
            ids(&without),
            [
                id(AttributeName::InputStyle),
                id(AttributeName::ClientWindow),
                id(AttributeName::FocusWindow)
            ]
        );
        let with = handler.create_ic_attributes(&client(), Some(xim::Point { x: 1, y: 2 }));
        assert_eq!(style_of(&with), 0x0402);
        assert_eq!(ids(&with).last(), Some(&id(AttributeName::PreeditAttributes)));
    }

    #[test]
    fn updating_the_spot_does_not_resend_creation_time_attributes() {
        let handler = XimHandler::new();
        let attributes = handler.spot_ic_attributes(&client(), xim::Point { x: 1, y: 2 });
        assert_eq!(
            ids(&attributes),
            [
                id(AttributeName::FocusWindow),
                id(AttributeName::PreeditAttributes)
            ]
        );
    }
}
