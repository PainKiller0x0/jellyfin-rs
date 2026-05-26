use std::io::Cursor;

use anyhow::Context;
use image::{
    DynamicImage, GenericImage, GenericImageView, ImageBuffer, ImageFormat, Rgba,
    imageops::FilterType,
};

pub struct ImageRequestOptions {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub quality: u8,
    pub format: EncodedImageFormat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EncodedImageFormat {
    Png,
    Jpeg,
    Webp,
}

impl EncodedImageFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }

    fn image_format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Webp => ImageFormat::WebP,
        }
    }
}

impl Default for ImageRequestOptions {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            quality: 90,
            format: EncodedImageFormat::Png,
        }
    }
}

pub fn process_image(bytes: &[u8], options: &ImageRequestOptions) -> anyhow::Result<Vec<u8>> {
    let image = image::load_from_memory(bytes).context("failed to decode source image")?;
    encode_image(resize_to_options(image, options), options)
}

pub fn create_collage(
    image_bytes: &[Vec<u8>],
    width: u32,
    height: u32,
    options: &ImageRequestOptions,
) -> anyhow::Result<Vec<u8>> {
    let mut canvas = ImageBuffer::from_pixel(width, height, Rgba([22, 27, 34, 255]));
    let valid_images = image_bytes
        .iter()
        .filter_map(|bytes| image::load_from_memory(bytes).ok())
        .take(4)
        .collect::<Vec<_>>();

    if valid_images.is_empty() {
        return create_placeholder(width, height, "jellyfin-rs", options);
    }

    let (columns, rows) = if valid_images.len() == 1 {
        (1, 1)
    } else {
        (2, 2)
    };
    let cell_width = width / columns;
    let cell_height = height / rows;

    for (index, source) in valid_images.iter().enumerate() {
        let x = (u32::try_from(index).unwrap_or_default() % columns) * cell_width;
        let y = (u32::try_from(index).unwrap_or_default() / columns) * cell_height;
        let resized = crop_resize(source, cell_width, cell_height);
        canvas
            .copy_from(&resized.to_rgba8(), x, y)
            .context("failed to draw collage cell")?;
    }

    encode_image(DynamicImage::ImageRgba8(canvas), options)
}

pub fn create_placeholder(
    width: u32,
    height: u32,
    seed: &str,
    options: &ImageRequestOptions,
) -> anyhow::Result<Vec<u8>> {
    let mut hash = 0u32;
    for byte in seed.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    let primary = [
        ((hash >> 16) & 0xff) as u8,
        ((hash >> 8) & 0xff) as u8,
        (hash & 0xff) as u8,
    ];
    let secondary = [
        primary[0].saturating_add(45),
        primary[1].saturating_add(30),
        primary[2].saturating_add(65),
    ];

    let mut image = ImageBuffer::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let mix = ((x + y) * 255 / (width + height).max(1)) as u8;
        *pixel = Rgba([
            lerp(primary[0], secondary[0], mix),
            lerp(primary[1], secondary[1], mix),
            lerp(primary[2], secondary[2], mix),
            255,
        ]);
    }

    draw_center_mark(&mut image);
    encode_image(DynamicImage::ImageRgba8(image), options)
}

fn resize_to_options(image: DynamicImage, options: &ImageRequestOptions) -> DynamicImage {
    match (options.width, options.height) {
        (Some(width), Some(height)) => {
            // Fit within the bounding box while preserving aspect ratio
            let (src_w, src_h) = image.dimensions();
            if src_w == 0 || src_h == 0 {
                return image;
            }
            let scale_w = width as f64 / src_w as f64;
            let scale_h = height as f64 / src_h as f64;
            let scale = scale_w.min(scale_h);
            let new_w = ((src_w as f64 * scale) as u32).max(1);
            let new_h = ((src_h as f64 * scale) as u32).max(1);
            image.resize(new_w, new_h, FilterType::Lanczos3)
        }
        (Some(width), None) => image.resize(width.max(1), u32::MAX, FilterType::Lanczos3),
        (None, Some(height)) => image.resize(u32::MAX, height.max(1), FilterType::Lanczos3),
        (None, None) => image,
    }
}

fn crop_resize(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    let (source_width, source_height) = image.dimensions();
    if source_width == 0 || source_height == 0 {
        return DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            width,
            height,
            Rgba([0, 0, 0, 255]),
        ));
    }

    let source_ratio = source_width as f32 / source_height as f32;
    let target_ratio = width as f32 / height as f32;
    let (crop_width, crop_height) = if source_ratio > target_ratio {
        ((source_height as f32 * target_ratio) as u32, source_height)
    } else {
        (source_width, (source_width as f32 / target_ratio) as u32)
    };
    let x = (source_width.saturating_sub(crop_width)) / 2;
    let y = (source_height.saturating_sub(crop_height)) / 2;
    image
        .crop_imm(x, y, crop_width.max(1), crop_height.max(1))
        .resize_exact(width, height, FilterType::Lanczos3)
}

fn encode_image(image: DynamicImage, options: &ImageRequestOptions) -> anyhow::Result<Vec<u8>> {
    let _quality = options.quality;
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), options.format.image_format())
        .context("failed to encode image")?;
    Ok(bytes)
}

fn lerp(start: u8, end: u8, mix: u8) -> u8 {
    let start = u16::from(start);
    let end = u16::from(end);
    let mix = u16::from(mix);
    (((start * (255 - mix)) + (end * mix)) / 255) as u8
}

fn draw_center_mark(image: &mut ImageBuffer<Rgba<u8>, Vec<u8>>) {
    let (width, height) = image.dimensions();
    let mark_width = (width / 3).max(12);
    let mark_height = (height / 5).max(8);
    let start_x = width.saturating_sub(mark_width) / 2;
    let start_y = height.saturating_sub(mark_height) / 2;
    for x in start_x..(start_x + mark_width).min(width) {
        for y in start_y..(start_y + mark_height).min(height) {
            if x == start_x
                || x + 1 == (start_x + mark_width).min(width)
                || y == start_y
                || y + 1 == (start_y + mark_height).min(height)
            {
                image.put_pixel(x, y, Rgba([255, 255, 255, 180]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_placeholder_png() {
        let bytes = create_placeholder(64, 36, "test", &ImageRequestOptions::default()).unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
    }

    #[test]
    fn creates_collage_from_generated_images() {
        let options = ImageRequestOptions::default();
        let first = create_placeholder(32, 32, "first", &options).unwrap();
        let second = create_placeholder(32, 32, "second", &options).unwrap();
        let collage = create_collage(&[first, second], 80, 45, &options).unwrap();
        assert!(collage.starts_with(b"\x89PNG"));
    }
}
