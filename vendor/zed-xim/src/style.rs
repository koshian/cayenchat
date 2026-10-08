use alloc::vec::Vec;

use xim_parser::InputStyle;

/// Picks the input style to create the input context with from the raw
/// `XNQueryInputStyle` reply (u16 count, u16 padding, then u32 styles).
///
/// The style is fixed at creation time, so it must be one the server offered:
/// preedit callbacks with no status area, falling back to the previous
/// hard-coded value when the reply is unusable.
pub fn choose_input_style(raw: Option<&[u8]>) -> InputStyle {
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
    .iter()
    .copied()
    .find(|style| offered.contains(&style.bits()))
    .unwrap_or(InputStyle::PREEDIT_CALLBACKS)
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
        assert_eq!(
            choose_input_style(Some(&[1])),
            InputStyle::PREEDIT_CALLBACKS
        );
    }
}
