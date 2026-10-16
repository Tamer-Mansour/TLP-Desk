//! Clipboard content <-> protocol messages: size caps, PNG compression, validation.
//!
//! Every byte from the peer is hostile: sizes are checked against the caps *before* anything
//! is allocated, PNG headers must agree with the announced size, and nothing here panics.

use std::io::Cursor;

use tlp_desk_proto::{encode_frame, ClipboardData, Message, ProtoError};

use super::error::ClipboardError;
use super::{ClipContent, ClipImage, ClipKind, ClipboardConfig};

/// Framed messages up to this size go on the control stream; larger ones on a
/// `StreamKind::Clipboard` side stream (protocol.md: "larger than about 1 MiB").
pub const INLINE_MAX: usize = 1 << 20;

/// Decoded RGBA size of a `width x height` image, `None` on overflow.
fn rgba_len(width: u32, height: u32) -> Option<u64> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)
}

fn too_large(kind: &'static str, size: u64, limit: u64) -> ClipboardError {
    ClipboardError::TooLarge { kind, size, limit }
}

fn len64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Checks an image's dimensions and buffer against `cap` (decoded RGBA bytes).
fn check_image(
    width: u32,
    height: u32,
    rgba_bytes: Option<usize>,
    cap: u64,
) -> Result<u64, ClipboardError> {
    if width == 0 || height == 0 {
        return Err(ClipboardError::BadImage("zero width or height".into()));
    }
    let need = rgba_len(width, height).ok_or_else(|| too_large("image", u64::MAX, cap))?;
    if need > cap {
        return Err(too_large("image", need, cap));
    }
    if let Some(have) = rgba_bytes {
        if len64(have) != need {
            return Err(ClipboardError::BadImage(format!(
                "{width}x{height} needs {need} RGBA bytes, got {have}"
            )));
        }
    }
    Ok(need)
}

/// The kind of clipboard content a message carries, if it is one this feature syncs.
pub(crate) fn kind_of(msg: &Message) -> Option<ClipKind> {
    match msg {
        Message::ClipboardData(ClipboardData::Text(_)) => Some(ClipKind::Text),
        Message::ClipboardData(_) => Some(ClipKind::Image),
        Message::ClipboardHtml { .. } => Some(ClipKind::Html),
        _ => None,
    }
}

/// Builds the message for `content`. `Ok(None)` for content this feature does not send
/// (file lists belong to the file-transfer feature).
pub(crate) fn to_message(
    content: &ClipContent,
    cfg: &ClipboardConfig,
) -> Result<Option<Message>, ClipboardError> {
    Ok(Some(match content {
        ClipContent::Text(t) => {
            let n = len64(t.len());
            if n > cfg.max_text_bytes {
                return Err(too_large("text", n, cfg.max_text_bytes));
            }
            Message::ClipboardData(ClipboardData::Text(t.clone()))
        }
        ClipContent::Html { html, alt_text } => {
            let n = len64(html.len()).saturating_add(len64(alt_text.len()));
            if n > cfg.max_html_bytes {
                return Err(too_large("html", n, cfg.max_html_bytes));
            }
            Message::ClipboardHtml {
                html: html.clone(),
                alt_text: alt_text.clone(),
            }
        }
        ClipContent::Image(img) => {
            check_image(
                img.width,
                img.height,
                Some(img.rgba.len()),
                cfg.max_image_bytes,
            )?;
            Message::ClipboardData(ClipboardData::Png {
                width: img.width,
                height: img.height,
                png: encode_png(img)?,
            })
        }
        ClipContent::Files(_) => return Ok(None),
    }))
}

/// Serializes `msg` as one frame and says whether it is small enough for the control stream.
/// A message over the protocol's frame limit is [`ClipboardError::TooLarge`].
pub(crate) fn frame(msg: &Message, kind: ClipKind) -> Result<(Vec<u8>, bool), ClipboardError> {
    match encode_frame(msg) {
        Ok(f) => {
            let inline = f.len() <= INLINE_MAX;
            Ok((f, inline))
        }
        Err(ProtoError::FrameTooLarge(n)) => Err(too_large(
            kind_name(kind),
            len64(n),
            len64(tlp_desk_proto::MAX_FRAME_LEN),
        )),
        Err(e) => Err(e.into()),
    }
}

fn kind_name(kind: ClipKind) -> &'static str {
    match kind {
        ClipKind::Text => "text",
        ClipKind::Html => "html",
        ClipKind::Image => "image",
        ClipKind::Files => "files",
    }
}

/// Turns a received message into clipboard content, enforcing the caps. `Ok(None)` for
/// messages that are not clipboard content.
pub(crate) fn from_message(
    msg: Message,
    cfg: &ClipboardConfig,
) -> Result<Option<ClipContent>, ClipboardError> {
    Ok(Some(match msg {
        Message::ClipboardData(ClipboardData::Text(t)) => {
            let n = len64(t.len());
            if n > cfg.max_text_bytes {
                return Err(too_large("text", n, cfg.max_text_bytes));
            }
            ClipContent::Text(t)
        }
        Message::ClipboardData(ClipboardData::Image {
            width,
            height,
            rgba,
        }) => {
            check_image(width, height, Some(rgba.len()), cfg.max_image_bytes)?;
            ClipContent::Image(ClipImage {
                width,
                height,
                rgba,
            })
        }
        Message::ClipboardData(ClipboardData::Png { width, height, png }) => {
            ClipContent::Image(decode_png(width, height, &png, cfg.max_image_bytes)?)
        }
        Message::ClipboardHtml { html, alt_text } => {
            let n = len64(html.len()).saturating_add(len64(alt_text.len()));
            if n > cfg.max_html_bytes {
                return Err(too_large("html", n, cfg.max_html_bytes));
            }
            ClipContent::Html { html, alt_text }
        }
        _ => return Ok(None),
    }))
}

fn png_err(e: impl std::fmt::Display) -> ClipboardError {
    ClipboardError::BadImage(e.to_string())
}

/// PNG-encodes an RGBA8 image (fast compression: clipboard sync is interactive).
pub(crate) fn encode_png(img: &ClipImage) -> Result<Vec<u8>, ClipboardError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().map_err(png_err)?;
        writer.write_image_data(&img.rgba).map_err(png_err)?;
        writer.finish().map_err(png_err)?;
    }
    Ok(out)
}

/// Decodes a PNG announced as `width x height` into RGBA8. The size is checked against `cap`
/// before decoding, and the decoder itself is limited to that budget.
pub(crate) fn decode_png(
    width: u32,
    height: u32,
    data: &[u8],
    cap: u64,
) -> Result<ClipImage, ClipboardError> {
    let need = check_image(width, height, None, cap)?;
    let need_usize = usize::try_from(need).map_err(|_| too_large("image", need, cap))?;
    // Room for the output buffer plus the decoder's own (row-sized) scratch space.
    let limits = png::Limits {
        bytes: need_usize.saturating_mul(2).saturating_add(1 << 20),
    };
    let mut dec = png::Decoder::new_with_limits(Cursor::new(data), limits);
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(png_err)?;
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if (w, h) != (width, height) {
        return Err(ClipboardError::BadImage(format!(
            "PNG is {w}x{h}, announced {width}x{height}"
        )));
    }
    let (color, depth) = reader.output_color_type();
    if depth != png::BitDepth::Eight {
        return Err(ClipboardError::BadImage("unsupported PNG bit depth".into()));
    }
    let channels = match color {
        png::ColorType::Grayscale => 1usize,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => {
            return Err(ClipboardError::BadImage("unexpanded palette PNG".into()))
        }
    };
    let buf_len = reader
        .output_buffer_size()
        .ok_or_else(|| too_large("image", u64::MAX, cap))?;
    // At most 4 bytes per pixel after normalization, so this is bounded by `need`, but check
    // anyway before allocating.
    if len64(buf_len) > need.saturating_add(len64(h as usize)) {
        return Err(too_large("image", len64(buf_len), cap));
    }
    let mut buf = vec![0u8; buf_len];
    let out = reader.next_frame(&mut buf).map_err(png_err)?;
    let row = (w as usize).saturating_mul(channels);
    let line = out.line_size.max(row);
    let mut rgba = Vec::with_capacity(need_usize);
    for y in 0..h as usize {
        let start = y.saturating_mul(line);
        let src = buf
            .get(start..start.saturating_add(row))
            .ok_or_else(|| ClipboardError::BadImage("truncated PNG rows".into()))?;
        match channels {
            4 => rgba.extend_from_slice(src),
            3 => src
                .chunks_exact(3)
                .for_each(|p| rgba.extend_from_slice(&[p[0], p[1], p[2], 255])),
            2 => src
                .chunks_exact(2)
                .for_each(|p| rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]])),
            _ => src
                .iter()
                .for_each(|&g| rgba.extend_from_slice(&[g, g, g, 255])),
        }
    }
    if rgba.len() != need_usize {
        return Err(ClipboardError::BadImage(
            "PNG pixel data is incomplete".into(),
        ));
    }
    Ok(ClipImage {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ClipboardConfig {
        ClipboardConfig::default()
    }

    fn image(w: u32, h: u32) -> ClipImage {
        let rgba = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        ClipImage {
            width: w,
            height: h,
            rgba,
        }
    }

    /// Arabic with harakat, RLM/LRM, ZWJ/ZWNJ, CRLF and LF, emoji (incl. a ZWJ sequence).
    pub(crate) const ARABIC: &str = "مَرْحَبًا بِالْعَالَم\u{200F}!\u{200E}abc\r\nسطر\nلا\u{200C}مي\u{200D}ـه 👨\u{200D}👩\u{200D}👧 🇸🇦 ﷺ";

    #[test]
    fn arabic_text_round_trips_byte_exact() {
        let c = ClipContent::Text(ARABIC.into());
        let msg = to_message(&c, &cfg()).unwrap().unwrap();
        let (f, inline) = frame(&msg, ClipKind::Text).unwrap();
        assert!(inline);
        let mut dec = tlp_desk_proto::FrameDecoder::new();
        dec.push(&f);
        let back = from_message(dec.next_message().unwrap().unwrap(), &cfg())
            .unwrap()
            .unwrap();
        let ClipContent::Text(t) = &back else {
            panic!("not text")
        };
        assert_eq!(t.as_bytes(), ARABIC.as_bytes());
        assert_eq!(t.chars().count(), ARABIC.chars().count());
        assert!(t.contains("\r\n") && t.contains("سطر\nلا"));
    }

    #[test]
    fn html_round_trips_with_alt_text() {
        let c = ClipContent::Html {
            html: "<p dir=\"rtl\">مرحبا <b>بالعالم</b></p>".into(),
            alt_text: "مرحبا بالعالم\u{200F}".into(),
        };
        let msg = to_message(&c, &cfg()).unwrap().unwrap();
        assert_eq!(from_message(msg, &cfg()).unwrap(), Some(c));
    }

    #[test]
    fn image_round_trips_through_png_and_is_compressed() {
        let mut img = image(64, 32);
        // Mostly flat image compresses well.
        img.rgba.iter_mut().skip(4096).for_each(|b| *b = 9);
        let c = ClipContent::Image(img.clone());
        let msg = to_message(&c, &cfg()).unwrap().unwrap();
        let Message::ClipboardData(ClipboardData::Png { png, .. }) = &msg else {
            panic!("expected PNG")
        };
        assert!(png.len() < img.rgba.len() / 2, "png {} bytes", png.len());
        assert_eq!(from_message(msg, &cfg()).unwrap(), Some(c));
    }

    #[test]
    fn text_over_cap_is_refused_both_ways() {
        let small = ClipboardConfig {
            max_text_bytes: 8,
            ..cfg()
        };
        // "مرحبا" is 10 UTF-8 bytes.
        let c = ClipContent::Text("مرحبا".into());
        assert!(matches!(
            to_message(&c, &small),
            Err(ClipboardError::TooLarge {
                kind: "text",
                size: 10,
                limit: 8
            })
        ));
        let m = Message::ClipboardData(ClipboardData::Text("مرحبا".into()));
        assert!(matches!(
            from_message(m, &small),
            Err(ClipboardError::TooLarge { .. })
        ));
    }

    #[test]
    fn image_cap_is_checked_before_decoding() {
        // Announces 100000 x 100000 (40 GB) with a tiny body: refused without allocating.
        let m = Message::ClipboardData(ClipboardData::Png {
            width: 100_000,
            height: 100_000,
            png: vec![1, 2, 3],
        });
        assert!(matches!(
            from_message(m, &cfg()),
            Err(ClipboardError::TooLarge { kind: "image", .. })
        ));
        let m = Message::ClipboardData(ClipboardData::Png {
            width: u32::MAX,
            height: u32::MAX,
            png: vec![],
        });
        assert!(from_message(m, &cfg()).is_err());
        let big = ClipContent::Image(image(64, 64));
        let tiny = ClipboardConfig {
            max_image_bytes: 1000,
            ..cfg()
        };
        assert!(matches!(
            to_message(&big, &tiny),
            Err(ClipboardError::TooLarge { .. })
        ));
    }

    #[test]
    fn png_whose_header_disagrees_is_rejected() {
        let png = encode_png(&image(4, 4)).unwrap();
        assert!(matches!(
            decode_png(4, 5, &png, 1 << 20),
            Err(ClipboardError::BadImage(_))
        ));
        assert!(matches!(
            decode_png(0, 4, &png, 1 << 20),
            Err(ClipboardError::BadImage(_))
        ));
        // Garbage and truncated bodies are errors, not panics.
        assert!(decode_png(4, 4, b"not a png", 1 << 20).is_err());
        assert!(decode_png(4, 4, &png[..png.len() / 2], 1 << 20).is_err());
    }

    #[test]
    fn raw_image_must_match_its_size() {
        let m = Message::ClipboardData(ClipboardData::Image {
            width: 2,
            height: 2,
            rgba: vec![0; 15],
        });
        assert!(matches!(
            from_message(m, &cfg()),
            Err(ClipboardError::BadImage(_))
        ));
        let ok = Message::ClipboardData(ClipboardData::Image {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 4],
        });
        assert!(matches!(
            from_message(ok, &cfg()),
            Ok(Some(ClipContent::Image(_)))
        ));
    }

    #[test]
    fn gray_and_rgb_pngs_expand_to_rgba() {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[1, 2, 3, 4, 5, 6]).unwrap();
        }
        let img = decode_png(2, 1, &out, 1 << 20).unwrap();
        assert_eq!(img.rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Sixteen);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[0xFF, 0, 0x10, 0]).unwrap();
        }
        let img = decode_png(2, 1, &out, 1 << 20).unwrap();
        assert_eq!(img.rgba, vec![0xFF, 0xFF, 0xFF, 255, 0x10, 0x10, 0x10, 255]);
    }

    #[test]
    fn large_payloads_leave_the_control_stream() {
        let c = ClipContent::Text("ب".repeat(INLINE_MAX));
        let msg = to_message(&c, &cfg()).unwrap().unwrap();
        let (_, inline) = frame(&msg, ClipKind::Text).unwrap();
        assert!(!inline);
        let huge = Message::ClipboardData(ClipboardData::Text("x".repeat(17 << 20)));
        assert!(matches!(
            frame(&huge, ClipKind::Text),
            Err(ClipboardError::TooLarge { .. })
        ));
    }

    #[test]
    fn files_and_other_messages_are_not_clipboard_content() {
        assert_eq!(
            to_message(&ClipContent::Files(vec!["C:\\ملف.txt".into()]), &cfg()).unwrap(),
            None
        );
        assert_eq!(
            from_message(Message::RequestKeyframe, &cfg()).unwrap(),
            None
        );
        assert_eq!(kind_of(&Message::RequestKeyframe), None);
    }
}
