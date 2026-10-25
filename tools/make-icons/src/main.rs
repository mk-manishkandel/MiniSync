//! Builds minisync's app icons from one source picture: dark line art (two
//! devices and sync arrows) with "Mini Sync" written below, on white.
//!
//!   cargo run --release --manifest-path tools/make-icons/Cargo.toml
//!
//! 1. Turns the picture into ink-with-alpha (white becomes transparent).
//! 2. Separates the artwork from the text; small sizes use the artwork only.
//! 3. Places it on a white rounded tile with a faint border, so the icon is
//!    visible on light and dark backgrounds alike.
//! 4. Writes assets/minisync.ico (Windows), assets/minisync.icns (macOS),
//!    the tray icons and PNG previews.

use anyhow::{bail, Context, Result};
use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};
use std::path::PathBuf;

const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);
const BORDER: Rgba<u8> = Rgba([214, 218, 228, 255]);

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let src = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("assets/icon-source.png"));
    let out = PathBuf::from(args.get(2).map(String::as_str).unwrap_or("assets"));

    let img = image::open(&src).with_context(|| format!("opening {}", src.display()))?.to_rgba8();
    let (ink, navy) = to_ink(&img);
    let (art, full) = split_art_and_text(&ink)?;

    // Large sizes show the logo with its name; small sizes only the artwork,
    // as big as possible (text is unreadable at 16-48 px).
    let tile_full = tile(&full, 1024, 0.74, navy);
    let tile_art = tile(&art, 1024, 0.84, navy);

    // Windows: the tile fills the icon, with a small margin.
    let win_full = pad(&tile_full, 0.02);
    let win_art = pad(&tile_art, 0.02);
    let mut ico = ico::IconDir::new(ico::ResourceType::Icon);
    for (size, src) in [(16, &win_art), (24, &win_art), (32, &win_art), (48, &win_art), (64, &win_art), (256, &win_full)] {
        let img = resize(src, size);
        ico.add_entry(ico::IconDirEntry::encode(&ico::IconImage::from_rgba_data(size, size, img.into_raw()))?);
    }
    ico.write(std::fs::File::create(out.join("minisync.ico"))?)?;

    // macOS: Apple's grid puts an 824px shape on a 1024px canvas.
    let mac_margin = (1024.0 / 824.0 - 1.0) / 2.0;
    let mac_full = pad(&tile_full, mac_margin);
    let mac_art = pad(&tile_art, mac_margin);
    let mut icns = icns::IconFamily::new();
    for (size, src) in [(16, &mac_art), (32, &mac_art), (64, &mac_art), (128, &mac_full), (256, &mac_full), (512, &mac_full), (1024, &mac_full)] {
        let img = resize(src, size);
        let image = icns::Image::from_data(icns::PixelFormat::RGBA, size, size, img.into_raw())?;
        icns.add_icon(&image)?;
    }
    icns.write(std::fs::File::create(out.join("minisync.icns"))?)?;

    // Tray icons as raw RGBA so the app needs no image decoder.
    // Windows: the tile, artwork filling as much as possible (taskbars are
    // often dark, so bare dark lines would vanish). macOS: the line art as a
    // black "template" image that the menu bar recolours for light/dark mode.
    let tray_tile = tile(&art, 1024, 0.9, navy);
    std::fs::write(out.join("tray-32.rgba"), resize(&tray_tile, 32).into_raw())?;
    std::fs::write(out.join("tray-template-44.rgba"), template(&art, 44).into_raw())?;

    // Previews (the README uses preview-256.png).
    resize(&mac_full, 1024).save(out.join("icon-1024.png"))?;
    resize(&win_full, 256).save(out.join("preview-256.png"))?;
    resize(&win_art, 48).save(out.join("preview-48-notext.png"))?;
    resize(&win_art, 512).save(out.join("preview-512-notext.png"))?;
    let mut sheet = RgbaImage::from_pixel(760, 300, WHITE);
    let mut dark = RgbaImage::from_pixel(200, 300, Rgba([32, 32, 36, 255]));
    imageops::overlay(&mut sheet, &resize(&win_full, 256), 16, 22);
    imageops::overlay(&mut sheet, &resize(&win_art, 48), 300, 22);
    imageops::overlay(&mut sheet, &resize(&win_art, 32), 370, 30);
    imageops::overlay(&mut sheet, &resize(&win_art, 16), 420, 38);
    imageops::overlay(&mut dark, &resize(&win_art, 48), 20, 22);
    imageops::overlay(&mut dark, &resize(&tray_tile, 32), 90, 30);
    imageops::overlay(&mut dark, &resize(&win_art, 16), 140, 38);
    imageops::overlay(&mut sheet, &dark, 520, 0);
    sheet.save(out.join("preview-on-white.png"))?;
    println!("wrote minisync.ico, minisync.icns, tray icons and previews to {}", out.display());
    Ok(())
}

fn brightness(p: &Rgba<u8>) -> f32 {
    (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0
}

/// Every pixel becomes the logo colour with "how dark it was" as alpha, so
/// the white background disappears and edges stay smooth.
fn to_ink(img: &RgbaImage) -> (RgbaImage, Rgba<u8>) {
    // Logo colour: average of clearly dark pixels.
    let (mut sum, mut n) = ([0f64; 3], 0f64);
    for p in img.pixels().filter(|p| brightness(p) < 80.0) {
        for c in 0..3 {
            sum[c] += p[c] as f64;
        }
        n += 1.0;
    }
    let navy = if n > 0.0 {
        Rgba([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, 255])
    } else {
        Rgba([22, 25, 48, 255])
    };
    let navy_b = brightness(&navy);
    let mut out = RgbaImage::new(img.width(), img.height());
    for (x, y, p) in img.enumerate_pixels() {
        let mut a = ((245.0 - brightness(p)) / (245.0 - navy_b)).clamp(0.0, 1.0);
        if a < 0.04 {
            a = 0.0;
        }
        out.put_pixel(x, y, Rgba([navy[0], navy[1], navy[2], (a * 255.0).round() as u8]));
    }
    (out, navy)
}

/// Returns (artwork only, artwork + text), each cropped tightly. The artwork
/// and the text are the first and last horizontal bands of ink.
fn split_art_and_text(ink: &RgbaImage) -> Result<(RgbaImage, RgbaImage)> {
    let (w, h) = ink.dimensions();
    let row_has_ink = |y: u32| (0..w).any(|x| ink.get_pixel(x, y)[3] > 100);
    let mut bands: Vec<(u32, u32)> = Vec::new();
    let mut gap = u32::MAX;
    for y in 0..h {
        if row_has_ink(y) {
            match bands.last_mut() {
                Some(b) if gap <= 20 => b.1 = y,
                _ => bands.push((y, y)),
            }
            gap = 0;
        } else {
            gap = gap.saturating_add(1);
        }
    }
    if bands.is_empty() {
        bail!("no artwork found (expected dark line art on a light background)");
    }
    let art = crop_to_ink(ink, bands[0].0, bands[0].1);
    let full = crop_to_ink(ink, bands[0].0, bands.last().unwrap().1);
    if bands.len() < 2 {
        eprintln!("no separate text found; small icons use the whole picture");
    }
    Ok((art, full))
}

fn crop_to_ink(ink: &RgbaImage, top: u32, bottom: u32) -> RgbaImage {
    let w = ink.width();
    let (mut x0, mut x1) = (w, 0);
    for y in top..=bottom {
        for x in 0..w {
            if ink.get_pixel(x, y)[3] > 0 {
                x0 = x0.min(x);
                x1 = x1.max(x);
            }
        }
    }
    imageops::crop_imm(ink, x0, top, x1 - x0 + 1, bottom - top + 1).to_image()
}

/// The artwork centred on a white rounded square with a faint border.
/// `fill` is how much of the tile the artwork may take (0..1).
fn tile(art: &RgbaImage, size: u32, fill: f32, _navy: Rgba<u8>) -> RgbaImage {
    let s = size as f32;
    let radius = s * 0.225;
    let border = (s * 0.012).max(1.0);
    let mut t = RgbaImage::from_pixel(size, size, Rgba([0, 0, 0, 0]));
    for y in 0..size {
        for x in 0..size {
            // Signed distance to the rounded square's edge (negative = inside).
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let qx = (px - s / 2.0).abs() - (s / 2.0 - radius);
            let qy = (py - s / 2.0).abs() - (s / 2.0 - radius);
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius;
            let coverage = (0.5 - outside).clamp(0.0, 1.0);
            if coverage == 0.0 {
                continue;
            }
            // Border ring blends into the white inside.
            let inner = (-outside - border).clamp(0.0, 1.0);
            let c = |a: u8, b: u8| (a as f32 * (1.0 - inner) + b as f32 * inner).round() as u8;
            t.put_pixel(x, y, Rgba([c(BORDER[0], WHITE[0]), c(BORDER[1], WHITE[1]), c(BORDER[2], WHITE[2]), (coverage * 255.0) as u8]));
        }
    }
    let scale = (s * fill / art.width() as f32).min(s * fill / art.height() as f32);
    let (nw, nh) = ((art.width() as f32 * scale) as u32, (art.height() as f32 * scale) as u32);
    let big = imageops::resize(art, nw, nh, FilterType::Lanczos3);
    imageops::overlay(&mut t, &big, ((size - nw) / 2) as i64, ((size - nh) / 2) as i64);
    t
}

/// The artwork as black-with-alpha on a transparent square (macOS template).
fn template(art: &RgbaImage, size: u32) -> RgbaImage {
    let side = art.width().max(art.height());
    let mut square = RgbaImage::from_pixel(side, side, Rgba([0, 0, 0, 0]));
    let black: RgbaImage = RgbaImage::from_fn(art.width(), art.height(), |x, y| Rgba([0, 0, 0, art.get_pixel(x, y)[3]]));
    imageops::overlay(&mut square, &black, ((side - art.width()) / 2) as i64, ((side - art.height()) / 2) as i64);
    resize(&square, size)
}

/// Centres the image on a transparent square canvas with `margin` (fraction
/// of the image size) on every side.
fn pad(img: &RgbaImage, margin: f32) -> RgbaImage {
    let side = img.width().max(img.height());
    let canvas_side = (side as f32 * (1.0 + 2.0 * margin)).round() as u32;
    let mut canvas = RgbaImage::from_pixel(canvas_side, canvas_side, Rgba([0, 0, 0, 0]));
    imageops::overlay(&mut canvas, img, ((canvas_side - img.width()) / 2) as i64, ((canvas_side - img.height()) / 2) as i64);
    canvas
}

fn resize(img: &RgbaImage, size: u32) -> RgbaImage {
    imageops::resize(img, size, size, FilterType::Lanczos3)
}
