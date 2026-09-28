//! Default avatars drawn by the client for users who have none.
//!
//! A port of CayenChat's `defaultAvatar.js`: the first four UTF-16 code
//! units of the nickname (as shown, case included) are hashed with FNV-1a,
//! and the hash picks the background (8 Okabe–Ito hues, a light and a dark
//! set), the figure color from the opposite set (contrast of at least 4:1),
//! the calyx "hat" (4 shapes) and the eyes (4 kinds): 512 combinations. The
//! result is an SVG that GPUI rasterizes; nothing is fetched. Shown only
//! while "Show user avatars" is on.

const LIGHT_BG: [&str; 4] = ["#E69F00", "#56B4E9", "#F0E442", "#E893C2"];
const DARK_BG: [&str; 4] = ["#025E93", "#954000", "#036649", "#332288"];
const DEEP_FIG: [&str; 4] = ["#034C78", "#743102", "#00553D", "#3F3399"];
const PALE_FIG: [&str; 4] = ["#FFD79D", "#BBE5FF", "#E8E38A", "#FFCEE7"];

const CALYX: &str = "#2FA548";
const CALYX_EDGE: &str = "#0E3B1C";

const CAP: &str = "M18.5 23C20 15 26 11.5 32 11.5S44 15 45.5 23C43 21.5 40.5 21.5 38.5 23.5C36.5 21.5 34 21 32 23C30 21 27.5 21.5 25.5 23.5C23.5 21.5 21 21.5 18.5 23Z";
const CURLED_STEM: &str = "M32 13C31.5 8 35 4.5 40.5 4";

/// What a nickname's default avatar looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Params {
    pub bg: &'static str,
    pub fig: &'static str,
    pub hat: u8,
    pub eyes: u8,
}

fn fnv1a(units: impl Iterator<Item = u16>) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in units {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// The avatar for `nickname`, exactly as `avatarParams` in the JavaScript
/// (`nick.slice(0, 4)` counts UTF-16 code units).
pub fn params(nickname: &str) -> Params {
    let hash = fnv1a(nickname.encode_utf16().take(4));
    let bg = (hash & 7) as usize;
    let fig = ((hash >> 3) & 3) as usize;
    let light = bg < 4;
    Params {
        bg: if light { LIGHT_BG[bg] } else { DARK_BG[bg - 4] },
        fig: if light { DEEP_FIG[fig] } else { PALE_FIG[fig] },
        hat: ((hash >> 5) & 3) as u8,
        eyes: ((hash >> 7) & 3) as u8,
    }
}

fn stem() -> String {
    format!(
        r#"<path d="{CURLED_STEM}" fill="none" stroke="{CALYX}" stroke-width="3.6" stroke-linecap="round"/><path d="{CAP}"/>"#
    )
}

fn hat(kind: u8) -> String {
    match kind {
        0 => stem(),
        1 => format!(r#"<g transform="matrix(-1 0 0 1 64 0)">{}</g>"#, stem()),
        2 => format!(
            r#"<path d="M32 13V5" fill="none" stroke="{CALYX}" stroke-width="3.6" stroke-linecap="round"/><path d="M33 8C35 3.5 40 2.5 44 4C42 8 37.5 9.5 33 8Z"/><path d="{CAP}"/>"#
        ),
        _ => format!(r#"<g transform="rotate(-14 32 22)">{}</g>"#, stem()),
    }
}

fn eyes(kind: u8, bg: &str) -> String {
    match kind {
        0 => format!(
            r#"<g fill="{bg}"><circle cx="27" cy="32" r="2"/><circle cx="37" cy="32" r="2"/></g>"#
        ),
        1 => format!(
            r#"<ellipse cx="27" cy="31.5" rx="2" ry="3" fill="{bg}"/><ellipse cx="37" cy="31.5" rx="2" ry="3" fill="{bg}"/>"#
        ),
        2 => format!(
            r#"<path d="M24.3 33.3Q27 29.3 29.7 33.3M34.3 33.3Q37 29.3 39.7 33.3" fill="none" stroke="{bg}" stroke-width="2" stroke-linecap="round"/>"#
        ),
        _ => format!(
            r#"<circle cx="27" cy="32" r="2" fill="{bg}"/><path d="M34.3 32.3H39.7" stroke="{bg}" stroke-width="2" stroke-linecap="round"/>"#
        ),
    }
}

/// The SVG of `defaultAvatar`, `size` pixels square.
pub fn svg(params: Params, size: u32) -> String {
    let Params { bg, fig, .. } = params;
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="{size}" height="{size}"><rect width="64" height="64" fill="{bg}"/><path d="M7 64C7 50 18 42 32 42S57 50 57 64Z" fill="{fig}"/><circle cx="32" cy="30" r="13" fill="{fig}" stroke="{bg}" stroke-width="2.5"/>{}<g fill="{CALYX}" stroke="{CALYX_EDGE}" stroke-width="1.2" stroke-linejoin="round">{}</g></svg>"#,
        eyes(params.eyes, bg),
        hat(params.hat),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values printed by `avatarParams` of defaultAvatar.js under Node.js.
    #[test]
    fn matches_the_javascript_reference() {
        for (nick, bg, fig, hat, eyes) in [
            ("koshian", "#025E93", "#FFD79D", 2, 2),
            ("Koshian", "#025E93", "#FFD79D", 3, 0),
            ("bob", "#025E93", "#E8E38A", 2, 1),
            ("a", "#025E93", "#BBE5FF", 1, 2),
            ("", "#954000", "#FFD79D", 2, 3),
            ("こしあん", "#025E93", "#BBE5FF", 2, 0),
            ("😀cat", "#E69F00", "#034C78", 3, 3),
            ("[away]x", "#56B4E9", "#3F3399", 2, 0),
            ("cayenchat_user", "#332288", "#BBE5FF", 1, 1),
        ] {
            assert_eq!(params(nick), Params { bg, fig, hat, eyes }, "{nick}");
        }
        // Only the first four code units count.
        assert_eq!(params("cayenchat_user"), params("caye"));
    }

    #[test]
    fn the_svg_is_the_javascript_drawing() {
        let svg = svg(params("bob"), 32);
        assert!(svg.starts_with(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="32" height="32">"#
        ));
        assert!(svg.contains(r##"<rect width="64" height="64" fill="#025E93"/>"##));
        assert!(svg.contains(r##"<ellipse cx="27" cy="31.5" rx="2" ry="3" fill="#025E93"/>"##));
        assert!(
            svg.contains(r#"<path d="M32 13V5""#),
            "straight stem with a leaf"
        );
        assert!(svg.ends_with("</g></svg>"));
        // Every combination is well formed enough to count its elements.
        for hat_kind in 0..4 {
            for eye_kind in 0..4 {
                let svg = super::svg(
                    Params {
                        bg: DARK_BG[0],
                        fig: PALE_FIG[0],
                        hat: hat_kind,
                        eyes: eye_kind,
                    },
                    32,
                );
                assert_eq!(svg.matches("<g").count(), svg.matches("</g>").count());
            }
        }
    }
}
