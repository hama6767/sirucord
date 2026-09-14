//! Offline participant card rendering. The bundled font avoids runtime font downloads.
use ab_glyph::{Font, FontRef, ScaleFont, point};
use anyhow::{Context, Result, ensure};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use std::io::Cursor;

use crate::avatars::Avatar;

const FONT: &[u8] = include_bytes!("../assets/fonts/NotoSansJP-Regular.otf");
const PADDING: u32 = 24;
const GAP: u32 = 16;
const CARD_WIDTH: u32 = 304;
const CARD_HEIGHT: u32 = 176;
const HEADER: u32 = 88;
const FOOTER: u32 = 24;
const BACKGROUND: [u8; 4] = [17, 22, 30, 255];
const SURFACE: [u8; 4] = [29, 37, 49, 255];
const TEXT: [u8; 4] = [236, 240, 246, 255];
const MUTED: [u8; 4] = [163, 175, 193, 255];

fn layout(count: usize) -> Result<(u32, u32, u32)> {
    ensure!(
        (2..=256).contains(&count),
        "Card requires 2–256 participants"
    );
    let columns = match count {
        2 | 4 => 2,
        3 | 5..=9 => 3,
        _ => 4,
    };
    let rows = (count as u32).div_ceil(columns);
    Ok((
        columns,
        PADDING * 2 + columns * CARD_WIDTH + (columns - 1) * GAP,
        HEADER + rows * CARD_HEIGHT + (rows - 1) * GAP + FOOTER,
    ))
}

fn blend(canvas: &mut RgbaImage, x: u32, y: u32, color: [u8; 4], coverage: f32) {
    if x >= canvas.width() || y >= canvas.height() {
        return;
    }
    let alpha = coverage.clamp(0.0, 1.0) * color[3] as f32 / 255.0;
    let pixel = canvas.get_pixel_mut(x, y);
    for i in 0..3 {
        pixel[i] = (pixel[i] as f32 * (1.0 - alpha) + color[i] as f32 * alpha).round() as u8;
    }
}

fn rounded(canvas: &mut RgbaImage, rect: (u32, u32, u32, u32), radius: f32, color: [u8; 4]) {
    let (left, top, width, height) = rect;
    for y in 0..height {
        for x in 0..width {
            let dx = (radius - (x as f32 + 0.5).min(width as f32 - x as f32 - 0.5)).max(0.0);
            let dy = (radius - (y as f32 + 0.5).min(height as f32 - y as f32 - 0.5)).max(0.0);
            blend(
                canvas,
                left + x,
                top + y,
                color,
                (radius + 0.5 - dx.hypot(dy)).clamp(0.0, 1.0),
            );
        }
    }
}

// ab_glyph's scale measures ascent-to-descent; use em size for predictable Japanese text.
fn scale(font: &FontRef<'_>, size: f32) -> f32 {
    size * font.height_unscaled() / font.units_per_em().unwrap_or(1000.0)
}

fn width(font: &FontRef<'_>, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(scale(font, size));
    let mut previous = None;
    let mut width = 0.0;
    for c in text.chars() {
        let glyph = scaled.glyph_id(c);
        if let Some(last) = previous {
            width += scaled.kern(last, glyph);
        }
        width += scaled.h_advance(glyph);
        previous = Some(glyph);
    }
    width
}

fn fit(font: &FontRef<'_>, text: &str, size: f32, max_width: f32) -> String {
    let mut text: String = text
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if font.glyph_id(c).0 == 0 { '□' } else { c })
        .collect();
    if width(font, &text, size) <= max_width {
        return text;
    }
    while !text.is_empty() && width(font, &format!("{text}…"), size) > max_width {
        text.pop();
    }
    text.push('…');
    text
}

fn text(
    canvas: &mut RgbaImage,
    font: &FontRef<'_>,
    value: &str,
    origin: (u32, u32),
    size: f32,
    max_width: f32,
    color: [u8; 4],
) {
    let value = fit(font, value, size, max_width);
    let scaled = font.as_scaled(scale(font, size));
    let mut cursor = origin.0 as f32;
    let mut previous = None;
    for c in value.chars() {
        let id = scaled.glyph_id(c);
        if let Some(last) = previous {
            cursor += scaled.kern(last, id);
        }
        let glyph = id.with_scale_and_position(
            scaled.scale(),
            point(cursor, origin.1 as f32 + scaled.ascent()),
        );
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|x, y, coverage| {
                let px = x as i32 + bounds.min.x as i32;
                let py = y as i32 + bounds.min.y as i32;
                if px >= 0 && py >= 0 && (px as f32) < origin.0 as f32 + max_width {
                    blend(canvas, px as u32, py as u32, color, coverage);
                }
            });
        }
        cursor += scaled.h_advance(id);
        previous = Some(id);
    }
}

/// Render only supplied, already validated images; this function performs no network access.
pub fn render(people: &[Avatar], images: &[RgbaImage]) -> Result<Vec<u8>> {
    ensure!(
        people.len() == images.len(),
        "Participant/image count mismatch"
    );
    let (columns, width, height) = layout(people.len())?;
    let font = FontRef::try_from_slice(FONT).context("Bundled Japanese font is invalid")?;
    let mut canvas = RgbaImage::from_pixel(width, height, Rgba(BACKGROUND));
    text(
        &mut canvas,
        &font,
        "通話のようす",
        (PADDING, 12),
        26.0,
        260.0,
        TEXT,
    );
    let live = people.iter().filter(|p| p.streaming).count();
    let summary = format!("{}人参加  ·  配信中 {}人", people.len(), live);
    text(
        &mut canvas,
        &font,
        &summary,
        (PADDING, 49),
        17.0,
        (width - 48) as f32,
        MUTED,
    );
    for (index, (person, avatar)) in people.iter().zip(images).enumerate() {
        let left = PADDING + index as u32 % columns * (CARD_WIDTH + GAP);
        let top = HEADER + index as u32 / columns * (CARD_HEIGHT + GAP);
        rounded(
            &mut canvas,
            (left, top, CARD_WIDTH, CARD_HEIGHT),
            16.0,
            SURFACE,
        );
        let portrait =
            image::imageops::resize(avatar, 64, 64, image::imageops::FilterType::Triangle);
        for (x, y, pixel) in portrait.enumerate_pixels() {
            let distance = (x as f32 + 0.5 - 32.0).hypot(y as f32 + 0.5 - 32.0);
            blend(
                &mut canvas,
                left + 20 + x,
                top + 20 + y,
                pixel.0,
                (32.5 - distance).clamp(0.0, 1.0),
            );
        }
        if person.streaming {
            rounded(
                &mut canvas,
                (left + 104, top + 34, 98, 30),
                15.0,
                [72, 35, 44, 255],
            );
            rounded(
                &mut canvas,
                (left + 115, top + 45, 8, 8),
                4.0,
                [255, 110, 124, 255],
            );
            text(
                &mut canvas,
                &font,
                "配信中",
                (left + 130, top + 36),
                16.0,
                64.0,
                [255, 163, 173, 255],
            );
        } else {
            text(
                &mut canvas,
                &font,
                "通話中",
                (left + 104, top + 36),
                16.0,
                100.0,
                MUTED,
            );
        }
        text(
            &mut canvas,
            &font,
            &person.name,
            (left + 20, top + 94),
            23.0,
            (CARD_WIDTH - 40) as f32,
            TEXT,
        );
        if let Some(game) = &person.game {
            text(
                &mut canvas,
                &font,
                &format!("プレイ中：{game}"),
                (left + 20, top + 133),
                16.0,
                (CARD_WIDTH - 40) as f32,
                MUTED,
            );
        }
    }
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(canvas).write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_font_and_long_labels_fit_without_overflow() {
        let font = FontRef::try_from_slice(FONT).unwrap();
        for c in "配信中通話参加汁開始名前…□".chars() {
            assert_ne!(font.glyph_id(c).0, 0);
        }
        let label = fit(
            &font,
            "とても長い日本語のゲームタイトルが入っても隣のカードに重ならない",
            23.0,
            264.0,
        );
        assert!(label.ends_with('…'));
        assert!(width(&font, &label, 23.0) <= 264.0);
        assert_eq!(fit(&font, "Minecraft", 16.0, 264.0), "Minecraft");
    }

    #[test]
    fn cards_keep_every_avatar_in_order_and_status_changes_pixels() {
        let mut people: Vec<_> = (0..10)
            .map(|n| Avatar {
                name: format!("参加者{n}"),
                url: String::new(),
                streaming: n % 2 == 0,
                game: Some("Minecraft".into()),
            })
            .collect();
        let images: Vec<_> = (0..10)
            .map(|n| RgbaImage::from_pixel(64, 64, Rgba([n, 80, 120, 255])))
            .collect();
        let first = render(&people, &images).unwrap();
        let pixels = image::load_from_memory(&first).unwrap().into_rgba8();
        for n in 0..10 {
            assert_eq!(
                pixels.get_pixel(
                    PADDING + n % 4 * (CARD_WIDTH + GAP) + 52,
                    HEADER + n / 4 * (CARD_HEIGHT + GAP) + 52
                )[0],
                n as u8
            );
        }
        people[0].streaming = false;
        let not_streaming = render(&people, &images).unwrap();
        assert_ne!(first, not_streaming);
        people[0].game = Some("別のゲーム".into());
        assert_ne!(not_streaming, render(&people, &images).unwrap());
        assert!(render(&people, &[]).is_err());
        assert!(render(&[], &[]).is_err());
        for count in 2..=256 {
            let (_, width, height) = layout(count).unwrap();
            assert!(width * height <= 16_777_216);
        }
    }
}
