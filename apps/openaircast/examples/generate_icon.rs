use std::{
    env,
    error::Error,
    fs::File,
    path::{Path, PathBuf},
};

use ico::{IconDir, IconDirEntry, IconImage, ResourceType};
use image::{imageops::FilterType, ImageBuffer, Rgba, RgbaImage};

const SUPERSAMPLE: u32 = 4;
const ROUTE_BLUE: Rgba<u8> = Rgba([0x5E, 0x72, 0xD6, 0xFF]);
const STREAMING_TEAL: Rgba<u8> = Rgba([0x51, 0xB9, 0xAF, 0xFF]);
const CANVAS: Rgba<u8> = Rgba([0xF5, 0xF7, 0xFB, 0xFF]);
const TRANSPARENT: Rgba<u8> = Rgba([0, 0, 0, 0]);

fn main() -> Result<(), Box<dyn Error>> {
    let args = env::args_os().collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: generate_icon <output.ico> <output.png>".into());
    }

    let ico_path = PathBuf::from(&args[1]);
    let png_path = PathBuf::from(&args[2]);
    write_ico(&ico_path)?;
    render_icon(256).save(png_path)?;
    Ok(())
}

fn write_ico(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut dir = IconDir::new(ResourceType::Icon);
    for size in [16, 20, 24, 32] {
        let image = render_icon(size);
        let entry = IconDirEntry::encode(&IconImage::from_rgba_data(size, size, image.into_raw()))?;
        dir.add_entry(entry);
    }
    dir.write(File::create(path)?)?;
    Ok(())
}

fn render_icon(size: u32) -> RgbaImage {
    let source_size = size * SUPERSAMPLE;
    image::imageops::resize(
        &render_vector(source_size),
        size,
        size,
        FilterType::Lanczos3,
    )
}

fn render_vector(size: u32) -> RgbaImage {
    let mut image = ImageBuffer::from_pixel(size, size, TRANSPARENT);
    let width = size as f32;
    let center_y = width * 0.50;

    draw_rounded_line(
        &mut image,
        width * 0.39,
        width * 0.62,
        center_y,
        width * 0.04,
        ROUTE_BLUE,
    );
    draw_circle(&mut image, width * 0.27, center_y, width * 0.16, ROUTE_BLUE);
    draw_circle(
        &mut image,
        width * 0.73,
        center_y,
        width * 0.215,
        STREAMING_TEAL,
    );
    draw_signal_arc(
        &mut image,
        width * 0.73,
        center_y,
        width * 0.035,
        width * 0.0625,
    );
    draw_signal_arc(
        &mut image,
        width * 0.73,
        center_y,
        width * 0.17,
        width * 0.0625,
    );

    image
}

fn draw_rounded_line(
    image: &mut RgbaImage,
    start_x: f32,
    end_x: f32,
    center_y: f32,
    radius: f32,
    color: Rgba<u8>,
) {
    for y in 0..image.height() {
        for x in 0..image.width() {
            let x = x as f32 + 0.5;
            let y = y as f32 + 0.5;
            let nearest_x = x.clamp(start_x, end_x);
            if (x - nearest_x).mul_add(x - nearest_x, (y - center_y) * (y - center_y))
                <= radius * radius
            {
                image.put_pixel((x - 0.5) as u32, (y - 0.5) as u32, color);
            }
        }
    }
}

fn draw_circle(image: &mut RgbaImage, center_x: f32, center_y: f32, radius: f32, color: Rgba<u8>) {
    for y in 0..image.height() {
        for x in 0..image.width() {
            let dx = x as f32 + 0.5 - center_x;
            let dy = y as f32 + 0.5 - center_y;
            if dx.mul_add(dx, dy * dy) <= radius * radius {
                image.put_pixel(x, y, color);
            }
        }
    }
}

fn draw_signal_arc(image: &mut RgbaImage, center_x: f32, center_y: f32, radius: f32, stroke: f32) {
    for y in 0..image.height() {
        for x in 0..image.width() {
            let dx = x as f32 + 0.5 - center_x;
            let dy = y as f32 + 0.5 - center_y;
            let distance = dx.hypot(dy);
            let angle = dy.atan2(dx);
            if dx > 0.0
                && angle.abs() <= std::f32::consts::FRAC_PI_2 * 0.8
                && (distance - radius).abs() <= stroke / 2.0
            {
                image.put_pixel(x, y, CANVAS);
            }
        }
    }
}
