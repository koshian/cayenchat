// SPDX-License-Identifier: MPL-2.0
//! Channel log wallpapers (#928): a local image file made ready to lay behind
//! log text. The image is reduced and blurred (fine detail is lost); how much
//! of it shows is the UI's business (the log background color is laid over it
//! with an opacity, #950), so a change of color or opacity never loads again.
//! The blur is applied once, when the file is loaded; the caller keeps the
//! result and the UI only scales it.

use std::{fs::File, io::Read, path::Path};

use crate::{CancelFlag, Limits, LoadError, Thumbnail, decode};

/// Files larger than this are refused without reading further.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// The wallpaper is kept at most this many pixels on a side; it is drawn
/// blurred and scaled, so more would only cost memory.
const MAX_SIDE: u32 = 640;

/// How a wallpaper's colors are changed when it is loaded (#1047).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tone {
    /// -100 (black) to +100 (white); 0 leaves it. Out of range is capped.
    pub brightness: i8,
    /// Percent, 0 (gray) to 200; 100 leaves it. Out of range is capped.
    pub saturation: u8,
}

impl Tone {
    /// Changes nothing.
    pub const NEUTRAL: Self = Self {
        brightness: 0,
        saturation: 100,
    };

    fn parts(self) -> (f32, f32) {
        (
            f32::from(self.brightness.clamp(-100, 100)) / 100.,
            f32::from(self.saturation.min(200)) / 100.,
        )
    }
}

/// Reads `path` and returns the wallpaper reduced to at most 640 px a side,
/// blurred with `blur` (tenths of a sigma in pixels of that size; 0 is no
/// blur) and toned with `tone`. Blocking; run it on a background thread.
///
/// `cancel` is checked before the file is opened, after it is read, while
/// waiting for and after taking the decode gate (inside the decoder), and
/// before the blur. A read or a decode or blur already running is not
/// interrupted: a read blocked in the OS cannot be, and the decoder and the
/// blur have no checkpoints of their own, so a cancel takes effect at the
/// next boundary.
pub fn load(
    path: &Path,
    blur: u8,
    tone: Tone,
    cancel: &CancelFlag,
) -> Result<Thumbnail, LoadError> {
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    let file = File::open(path).map_err(|_| LoadError::Network)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| LoadError::Malformed)?;
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(LoadError::TooLarge);
    }
    let limits = Limits {
        thumbnail_width: MAX_SIDE,
        thumbnail_height: MAX_SIDE,
        ..Limits::default()
    };
    let mut thumbnail = decode::thumbnail(&bytes, &limits, cancel)?;
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    apply(&mut thumbnail, blur, tone);
    Ok(thumbnail)
}

/// File name extensions of the formats [`validate`] accepts, for a file
/// dialog to offer only those files. The content check stays the authority.
pub const FILE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Checks that `bytes` are a wallpaper image this app can draw (PNG, JPEG,
/// GIF or WebP within the decoder's size limits) and returns the file name
/// extension for its format. Decodes it, so run it off the UI thread.
pub fn validate(bytes: &[u8]) -> Result<&'static str, LoadError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(LoadError::TooLarge);
    }
    let extension = match decode::format(bytes)? {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::Gif => "gif",
        _ => "webp",
    };
    let limits = Limits {
        thumbnail_width: MAX_SIDE,
        thumbnail_height: MAX_SIDE,
        ..Limits::default()
    };
    decode::thumbnail(bytes, &limits, &CancelFlag::default())?;
    Ok(extension)
}

/// Blurs `thumbnail` (`blur` in tenths of a sigma), tones it and makes it
/// opaque.
fn apply(thumbnail: &mut Thumbnail, blur: u8, tone: Tone) {
    let pixels = std::mem::take(&mut thumbnail.bgra);
    let mut pixels = if blur == 0 {
        pixels
    } else {
        let image = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, pixels)
            .expect("thumbnail buffer matches its size");
        // The channels are blurred alike, whatever their order.
        image::imageops::blur(&image, f32::from(blur) / 10.0).into_raw()
    };
    let (brightness, saturation) = tone.parts();
    let toned = brightness != 0. || saturation != 1.;
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
        if toned {
            tone_pixel(pixel, brightness, saturation);
        }
    }
    thumbnail.bgra = pixels;
}

/// Saturation, then brightness, on the sRGB values of one BGRA pixel as they
/// are (0 to 1). Brightness is not an offset: it moves toward black or
/// white, so the picture's tones keep their order.
fn tone_pixel(pixel: &mut [u8; 4], brightness: f32, saturation: f32) {
    let [b, g, r] = [pixel[0], pixel[1], pixel[2]].map(|c| f32::from(c) / 255.);
    let gray = 0.299 * r + 0.587 * g + 0.114 * b;
    for (out, c) in pixel.iter_mut().zip([b, g, r]) {
        let c = (gray + (c - gray) * saturation).clamp(0., 1.);
        let c = if brightness < 0. {
            c * (1. + brightness)
        } else {
            c + (1. - c) * brightness
        };
        *out = (c * 255.).round().clamp(0., 255.) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkerboard(side: u32) -> Thumbnail {
        let mut bgra = Vec::new();
        for y in 0..side {
            for x in 0..side {
                let value = if (x + y) % 2 == 0 { 255 } else { 0 };
                bgra.extend_from_slice(&[value, value, value, 255]);
            }
        }
        Thumbnail {
            width: side,
            height: side,
            source_width: side,
            source_height: side,
            bgra,
        }
    }

    fn blurred(blur: u8) -> Vec<u8> {
        let mut image = checkerboard(32);
        apply(&mut image, blur, Tone::NEUTRAL);
        image.bgra
    }

    fn spread(bgra: &[u8]) -> u8 {
        let (low, high) = bgra.chunks(4).fold((255u8, 0u8), |(low, high), p| {
            (low.min(p[0]), high.max(p[0]))
        });
        high - low
    }

    #[test]
    fn no_blur_leaves_the_picture_as_it_was() {
        assert_eq!(blurred(0), checkerboard(32).bgra);
    }

    #[test]
    fn blur_smooths_the_picture_and_stays_opaque() {
        let result = blurred(30);
        assert_ne!(result, checkerboard(32).bgra);
        assert!(result.chunks(4).all(|p| p[3] == 255));
        // Black and white squares average out toward gray.
        let middle = result[(16 * 32 + 16) * 4];
        assert!((60..200).contains(&middle), "{middle}");
    }

    fn toned(tone: Tone) -> Vec<u8> {
        // BGRA: a blue-ish, a red-ish, a dark and a light pixel.
        let bgra = vec![
            200, 80, 30, 255, //
            20, 40, 220, 255, //
            0, 0, 0, 255, //
            255, 255, 255, 255,
        ];
        let mut image = Thumbnail {
            width: 4,
            height: 1,
            source_width: 4,
            source_height: 1,
            bgra: bgra.clone(),
        };
        apply(&mut image, 0, tone);
        image.bgra
    }

    #[test]
    fn the_neutral_tone_leaves_every_pixel_as_it_was() {
        assert_eq!(
            toned(Tone::NEUTRAL),
            vec![
                200, 80, 30, 255, 20, 40, 220, 255, 0, 0, 0, 255, 255, 255, 255, 255
            ]
        );
    }

    #[test]
    fn the_extremes_of_brightness_give_black_and_white() {
        let black = toned(Tone {
            brightness: -100,
            saturation: 100,
        });
        assert!(black.chunks(4).all(|p| p[..3] == [0, 0, 0] && p[3] == 255));
        let white = toned(Tone {
            brightness: 100,
            saturation: 100,
        });
        assert!(
            white
                .chunks(4)
                .all(|p| p[..3] == [255, 255, 255] && p[3] == 255)
        );
    }

    #[test]
    fn brightness_moves_toward_one_side_and_keeps_the_order() {
        let darker = toned(Tone {
            brightness: -50,
            saturation: 100,
        });
        // 200 * 0.5, and black stays black.
        assert_eq!(darker[0], 100);
        assert_eq!(darker[8], 0);
        let lighter = toned(Tone {
            brightness: 50,
            saturation: 100,
        });
        // 30 + (255 - 30) * 0.5 (red of the first pixel), black becomes gray.
        assert_eq!(lighter[2], 143);
        assert_eq!(lighter[8], 128);
        assert_eq!(lighter[12], 255);
    }

    #[test]
    fn zero_saturation_is_gray_and_the_weights_follow_red_green_blue() {
        let gray = toned(Tone {
            brightness: 0,
            saturation: 0,
        });
        assert!(gray.chunks(4).all(|p| p[0] == p[1] && p[1] == p[2]));
        // Red 30, green 80, blue 200: 0.299 * 30 + 0.587 * 80 + 0.114 * 200.
        assert_eq!(gray[0], 79);
        // More saturation moves the channels apart again.
        let vivid = toned(Tone {
            brightness: 0,
            saturation: 200,
        });
        assert!(vivid[0] > 200 && vivid[2] < 30);
    }

    #[test]
    fn a_tone_out_of_range_is_capped() {
        assert_eq!(
            toned(Tone {
                brightness: i8::MIN,
                saturation: 255
            }),
            toned(Tone {
                brightness: -100,
                saturation: 200
            })
        );
    }

    #[test]
    fn a_stronger_blur_changes_the_result_further() {
        assert!(spread(&blurred(30)) < spread(&blurred(10)));
        // A tenth steps are felt: a small blur still differs from none.
        assert_ne!(blurred(5), blurred(0));
    }

    #[test]
    fn validate_names_the_format_and_refuses_what_is_not_an_image() {
        let mut png = Vec::new();
        image::RgbaImage::new(4, 4)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(validate(&png), Ok("png"));
        assert_eq!(validate(b"not an image"), Err(LoadError::NotImage));
        // A cut-off file is malformed, not accepted on its header alone.
        assert!(validate(&png[..png.len() / 2]).is_err());
        let huge = vec![0u8; MAX_FILE_BYTES as usize + 1];
        assert_eq!(validate(&huge), Err(LoadError::TooLarge));
    }

    #[test]
    fn a_load_cancelled_while_waiting_for_the_decode_gate_does_not_decode() {
        let dir =
            std::env::temp_dir().join(format!("cayenchat-wall-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ok.png");
        image::RgbaImage::new(8, 8).save(&file).unwrap();
        let cancel = CancelFlag::default();
        // A slow decode elsewhere holds the gate; the load queues behind it.
        let gate = crate::decode::DECODE.lock().unwrap();
        let worker = {
            let (file, cancel) = (file.clone(), cancel.clone());
            std::thread::spawn(move || load(&file, 0, Tone::NEUTRAL, &cancel))
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!worker.is_finished());
        cancel.cancel();
        drop(gate);
        assert_eq!(worker.join().unwrap().unwrap_err(), LoadError::Cancelled);
        // Cancelled before it starts, it does not even open the file.
        assert_eq!(
            load(&dir.join("absent.png"), 0, Tone::NEUTRAL, &cancel).unwrap_err(),
            LoadError::Cancelled
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Times `apply` on a 640 px square picture (the largest kept):
    /// `cargo test --release -p cayenchat-media apply_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, run by hand in a release build"]
    fn apply_timing() {
        let mut image = checkerboard(640);
        image
            .bgra
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b ^= (i % 251) as u8);
        let tone = Tone {
            brightness: -40,
            saturation: 80,
        };
        for (label, blur, tone) in [
            ("neutral, no blur", 0, Tone::NEUTRAL),
            ("tone only", 0, tone),
            ("blur 30 only", 30, Tone::NEUTRAL),
            ("blur 30 and tone", 30, tone),
        ] {
            let mut times = (0..9)
                .map(|_| {
                    let mut copy = image.clone();
                    let start = std::time::Instant::now();
                    apply(&mut copy, blur, tone);
                    start.elapsed()
                })
                .collect::<Vec<_>>();
            times.sort();
            println!("{label}: median {:?}", times[4]);
        }
    }

    #[test]
    fn a_missing_or_foreign_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("cayenchat-wall-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cancel = CancelFlag::default();
        assert!(load(&dir.join("absent.png"), 3, Tone::NEUTRAL, &cancel).is_err());
        let text = dir.join("text.png");
        std::fs::write(&text, b"not an image").unwrap();
        assert_eq!(
            load(&text, 3, Tone::NEUTRAL, &cancel).unwrap_err(),
            LoadError::NotImage
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
