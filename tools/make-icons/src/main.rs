//! Builds minisync's app icons from one source picture.
//!
//!   cargo run --release --manifest-path tools/make-icons/Cargo.toml
//!
//! 1. Makes the white surround transparent (keeping the anti-aliased edge).
//! 2. Makes a "no text" variant for small sizes, with the artwork enlarged and centred.
//! 3. Writes assets/minisync.ico (Windows) and assets/minisync.icns (macOS),
//!    plus PNG previews.

use anyhow::{bail, Context, Result};
use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};
use std::collections::VecDeque;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let src = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("assets/icon-source.png"));
    let out = PathBuf::from(args.get(2).map(String::as_str).unwrap_or("assets"));

    let img = image::open(&src).with_context(|| format!("opening {}", src.display()))?.to_rgba8();
    let (shape, navy) = cut_out(img)?;
    let notext = without_text(&shape, navy);

    // Windows: the rounded square fills the icon, with a small margin.
    let win_full = pad_square(&shape, 0.02);
    let win_small = pad_square(&notext, 0.02);
    let mut ico = ico::IconDir::new(ico::ResourceType::Icon);
    for (size, src) in [(16, &win_small), (24, &win_small), (32, &win_small), (48, &win_small), (64, &win_small), (256, &win_full)] {
        let img = resize(src, size);
        ico.add_entry(ico::IconDirEntry::encode(&ico::IconImage::from_rgba_data(size, size, img.into_raw()))?);
    }
    ico.write(std::fs::File::create(out.join("minisync.ico"))?)?;

    // macOS: Apple's grid puts an 824px shape on a 1024px canvas.
    let mac_full = pad_square(&shape, (1024.0 / 824.0 - 1.0) / 2.0);
    let mac_small = pad_square(&notext, (1024.0 / 824.0 - 1.0) / 2.0);
    let mut icns = icns::IconFamily::new();
    for (size, src) in [(16, &mac_small), (32, &mac_small), (64, &mac_small), (128, &mac_full), (256, &mac_full), (512, &mac_full), (1024, &mac_full)] {
        let img = resize(src, size);
        let image = icns::Image::from_data(icns::PixelFormat::RGBA, size, size, img.into_raw())?;
        icns.add_icon(&image)?;
    }
    icns.write(std::fs::File::create(out.join("minisync.icns"))?)?;

    // Tray icons as raw RGBA so the app needs no image decoder.
    // Windows: the coloured no-text icon. macOS: a black glyph used as a
    // "template" image, which the menu bar recolours for light/dark mode.
    std::fs::write(out.join("tray-32.rgba"), resize(&win_small, 32).into_raw())?;
    std::fs::write(out.join("tray-template-44.rgba"), glyph(&mac_small, 44).into_raw())?;

    resize(&mac_full, 1024).save(out.join("icon-1024.png"))?;
    resize(&win_small, 48).save(out.join("preview-48-notext.png"))?;
    resize(&win_small, 512).save(out.join("preview-512-notext.png"))?;
    let mut on_white = RgbaImage::from_pixel(560, 300, Rgba([255, 255, 255, 255]));
    imageops::overlay(&mut on_white, &resize(&win_full, 256), 16, 22);
    imageops::overlay(&mut on_white, &resize(&win_small, 48), 300, 22);
    imageops::overlay(&mut on_white, &resize(&win_small, 32), 370, 30);
    imageops::overlay(&mut on_white, &resize(&win_small, 16), 420, 38);
    on_white.save(out.join("preview-on-white.png"))?;
    resize(&win_full, 256).save(out.join("preview-256.png"))?;
    println!("wrote minisync.ico, minisync.icns and previews to {}", out.display());
    Ok(())
}

fn brightness(p: &Rgba<u8>) -> u8 {
    ((p[0] as u32 + p[1] as u32 + p[2] as u32) / 3) as u8
}

/// Removes the light background around the rounded square. Returns the
/// tightly cropped shape and the shape's fill colour.
fn cut_out(mut img: RgbaImage) -> Result<(RgbaImage, Rgba<u8>)> {
    let (w, h) = img.dimensions();
    let idx = |x: u32, y: u32| (y * w + x) as usize;

    // Flood-fill the background from every border pixel.
    let mut bg = vec![false; (w * h) as usize];
    let mut queue = VecDeque::new();
    for x in 0..w {
        queue.push_back((x, 0));
        queue.push_back((x, h - 1));
    }
    for y in 0..h {
        queue.push_back((0, y));
        queue.push_back((w - 1, y));
    }
    while let Some((x, y)) = queue.pop_front() {
        if bg[idx(x, y)] || brightness(img.get_pixel(x, y)) < 128 {
            continue;
        }
        bg[idx(x, y)] = true;
        if x > 0 { queue.push_back((x - 1, y)); }
        if y > 0 { queue.push_back((x, y - 1)); }
        if x + 1 < w { queue.push_back((x + 1, y)); }
        if y + 1 < h { queue.push_back((x, y + 1)); }
    }
    if bg.iter().all(|b| !*b) {
        bail!("no light background found around the icon");
    }

    // Fill colour: a little inside the shape on the middle row.
    let row = h / 2;
    let edge = (0..w).find(|&x| !bg[idx(x, row)]).context("shape not found")?;
    let navy = *img.get_pixel((edge + 40).min(w - 1), row);
    let navy_b = brightness(&navy) as f32;

    // Pixels within 2px of the background are the anti-aliased rim: turn their
    // "how white is it" into transparency over the fill colour.
    let mut rim = bg.clone();
    for _ in 0..2 {
        let prev = rim.clone();
        for y in 0..h {
            for x in 0..w {
                if prev[idx(x, y)] {
                    continue;
                }
                let near = (x > 0 && prev[idx(x - 1, y)])
                    || (y > 0 && prev[idx(x, y - 1)])
                    || (x + 1 < w && prev[idx(x + 1, y)])
                    || (y + 1 < h && prev[idx(x, y + 1)]);
                if near {
                    rim[idx(x, y)] = true;
                }
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            if !rim[idx(x, y)] {
                continue;
            }
            let b = brightness(img.get_pixel(x, y)) as f32;
            let mut a = ((255.0 - b) / (255.0 - navy_b)).clamp(0.0, 1.0);
            if a < 0.03 {
                a = 0.0;
            }
            img.put_pixel(x, y, Rgba([navy[0], navy[1], navy[2], (a * 255.0).round() as u8]));
        }
    }

    // Crop to the visible shape.
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    let shape = imageops::crop_imm(&img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image();
    Ok((shape, navy))
}

/// Finds the artwork and the text as separate horizontal bands of light
/// pixels, removes the text and centres an enlarged copy of the artwork.
fn without_text(shape: &RgbaImage, navy: Rgba<u8>) -> RgbaImage {
    let (w, h) = shape.dimensions();
    let is_ink = |p: &Rgba<u8>| p[3] == 255 && brightness(p) > 160;

    let mut bands: Vec<(u32, u32)> = Vec::new();
    let mut gap = u32::MAX;
    for y in 0..h {
        let ink = (0..w).any(|x| is_ink(shape.get_pixel(x, y)));
        if ink {
            match bands.last_mut() {
                Some(b) if gap <= 15 => b.1 = y,
                _ => bands.push((y, y)),
            }
            gap = 0;
        } else {
            gap = gap.saturating_add(1);
        }
    }
    if bands.len() < 2 {
        eprintln!("could not separate text from artwork; small icons will keep the text");
        return shape.clone();
    }
    let art = bands[0];
    let text = *bands.last().unwrap();
    let cols = |(top, bottom): (u32, u32)| {
        let xs: Vec<u32> = (0..w)
            .filter(|&x| (top..=bottom).any(|y| is_ink(shape.get_pixel(x, y))))
            .collect();
        (*xs.first().unwrap(), *xs.last().unwrap())
    };
    let (ax0, ax1) = cols(art);
    let (tx0, tx1) = cols(text);
    let pad = 8;

    let art_img = imageops::crop_imm(
        shape,
        ax0 - pad,
        art.0 - pad,
        ax1 - ax0 + 1 + 2 * pad,
        art.1 - art.0 + 1 + 2 * pad,
    )
    .to_image();

    // Paint over both bands with the fill colour (opaque pixels only).
    let mut out = shape.clone();
    let x_from = ax0.min(tx0) - pad;
    let x_to = ax1.max(tx1) + pad;
    for y in (art.0 - pad)..=(text.1 + pad).min(h - 1) {
        for x in x_from..=x_to.min(w - 1) {
            if out.get_pixel(x, y)[3] == 255 {
                out.put_pixel(x, y, navy);
            }
        }
    }

    // Enlarge the artwork as much as fits in 78% of the shape, centred.
    let scale = (w as f32 * 0.78 / art_img.width() as f32).min(h as f32 * 0.78 / art_img.height() as f32);
    let nw = (art_img.width() as f32 * scale) as u32;
    let nh = (art_img.height() as f32 * scale) as u32;
    let big = imageops::resize(&art_img, nw, nh, FilterType::Lanczos3);
    let ox = (w - nw) / 2;
    let oy = (h - nh) / 2;
    for (x, y, p) in big.enumerate_pixels() {
        let dest = out.get_pixel_mut(ox + x, oy + y);
        if dest[3] == 255 {
            *dest = Rgba([p[0], p[1], p[2], 255]);
        }
    }
    out
}

/// Centres the image on a transparent square canvas with `margin` (fraction
/// of the image size) on every side.
fn pad_square(img: &RgbaImage, margin: f32) -> RgbaImage {
    let side = img.width().max(img.height());
    let canvas_side = (side as f32 * (1.0 + 2.0 * margin)).round() as u32;
    let mut canvas = RgbaImage::from_pixel(canvas_side, canvas_side, Rgba([0, 0, 0, 0]));
    let x = (canvas_side - img.width()) / 2;
    let y = (canvas_side - img.height()) / 2;
    imageops::overlay(&mut canvas, img, x as i64, y as i64);
    canvas
}

/// Keeps only the light line art (devices + arrows) as black-with-alpha,
/// cropped to fill the icon.
fn glyph(img: &RgbaImage, size: u32) -> RgbaImage {
    let mut g = RgbaImage::from_pixel(img.width(), img.height(), Rgba([0, 0, 0, 0]));
    let (mut x0, mut y0, mut x1, mut y1) = (img.width(), img.height(), 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        let b = brightness(p) as f32;
        let a = (((b - 60.0) / 160.0).clamp(0.0, 1.0) * p[3] as f32) as u8;
        if a > 0 {
            g.put_pixel(x, y, Rgba([0, 0, 0, a]));
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    let side = (x1 - x0).max(y1 - y0) + 1;
    let cropped = imageops::crop_imm(&g, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image();
    let mut square = RgbaImage::from_pixel(side, side, Rgba([0, 0, 0, 0]));
    imageops::overlay(&mut square, &cropped, ((side - cropped.width()) / 2) as i64, ((side - cropped.height()) / 2) as i64);
    resize(&square, size)
}

fn resize(img: &RgbaImage, size: u32) -> RgbaImage {
    imageops::resize(img, size, size, FilterType::Lanczos3)
}
