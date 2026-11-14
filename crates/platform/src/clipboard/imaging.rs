use std::io::{self, Cursor};

use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits,
    RgbaImage, codecs::png::PngEncoder,
};
use unikvm_core::clipboard::MAX_PNG_BYTES;

// Limit both axes and area before allocating decoded pixels.
const MAX_SIDE: u32 = 4096;
const MAX_PIXELS: u64 = 9 * 1024 * 1024;
pub(super) const MAX_NATIVE_BYTES: usize = 64 * 1024 * 1024;

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_NATIVE_BYTES as u64);
    limits
}

pub(super) fn decode(bytes: &[u8], format: ImageFormat) -> Option<RgbaImage> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits());
    decode_pixels(reader.into_decoder().ok()?)
}

fn decode_pixels(decoder: impl ImageDecoder) -> Option<RgbaImage> {
    let (width, height) = decoder.dimensions();
    if !dimensions_allowed(width, height) || decoder.total_bytes() > MAX_NATIVE_BYTES as u64 {
        return None;
    }
    DynamicImage::from_decoder(decoder)
        .ok()
        .map(DynamicImage::into_rgba8)
}

fn dimensions_allowed(width: u32, height: u32) -> bool {
    (1..=MAX_SIDE).contains(&width)
        && (1..=MAX_SIDE).contains(&height)
        && u64::from(width) * u64::from(height) <= MAX_PIXELS
}

pub(super) fn to_png(pixels: &RgbaImage) -> Option<Vec<u8>> {
    let mut bytes = BoundedPng(Vec::new());
    PngEncoder::new(&mut bytes)
        .write_image(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(bytes.0)
}

pub(super) fn normalize(bytes: &[u8], format: ImageFormat) -> Option<Vec<u8>> {
    to_png(&decode(bytes, format)?)
}

struct BoundedPng(Vec<u8>);

impl io::Write for BoundedPng {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_PNG_BYTES - self.0.len() {
            return Err(io::Error::other("clipboard PNG exceeds the size limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub(super) fn from_dib(bytes: &[u8]) -> Option<Vec<u8>> {
    use image::codecs::bmp::BmpDecoder;
    use windows_sys::Win32::Graphics::Gdi::{BI_BITFIELDS, BITMAPV5HEADER};

    let header = size_of::<BITMAPV5HEADER>();
    let field = |offset: usize| {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    };
    if field(0) == Some(header as u32)
        && field(16) == Some(BI_BITFIELDS)
        && field(32) == Some(0)
        && field(116) == Some(0)
    {
        // The DIB decoder assumes extra masks after this header. This packed
        // form already stores its masks inside the header.
        let (width, height) = (field(4)?, (field(8)? as i32).unsigned_abs());
        if !dimensions_allowed(width, height) {
            return None;
        }
        let size = u32::from(u16::from_le_bytes(bytes[14..16].try_into().ok()?));
        if !matches!(size, 16 | 32) {
            return None;
        }
        let stride = (width * size).div_ceil(32) * 4;
        let pixels = bytes.get(header..header + (stride * height) as usize)?;
        let dib = &bytes[..header + pixels.len()];
        let start = (14 + header) as u32;
        let mut bmp = Vec::with_capacity(14 + dib.len());
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&start.to_le_bytes());
        bmp.extend_from_slice(dib);
        return to_png(&decode_pixels(BmpDecoder::new(Cursor::new(bmp)).ok()?)?);
    }

    let decoder = BmpDecoder::new_without_file_header(Cursor::new(bytes)).ok()?;
    to_png(&decode_pixels(decoder)?)
}

#[cfg(target_os = "windows")]
pub(super) fn to_dib(pixels: &RgbaImage) -> Vec<u8> {
    use windows_sys::Win32::Graphics::Gdi::{BI_BITFIELDS, BITMAPV5HEADER, LCS_GM_IMAGES};

    // Packed BITMAPV5HEADER, then top-down BGRA pixels. No file header or palette.
    let mut data = vec![0; size_of::<BITMAPV5HEADER>() + pixels.as_raw().len()];
    let header = &mut data[..size_of::<BITMAPV5HEADER>()];
    for (offset, value) in [
        (0, size_of::<BITMAPV5HEADER>() as u32),
        (4, pixels.width()),
        (8, (-(pixels.height() as i32)) as u32),
        (16, BI_BITFIELDS),
        (20, pixels.as_raw().len() as u32),
        (40, 0x00ff_0000),
        (44, 0x0000_ff00),
        (48, 0x0000_00ff),
        (52, 0xff00_0000),
        (56, 0x7352_4742), // LCS_sRGB
        (108, LCS_GM_IMAGES as u32),
    ] {
        header[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    header[12..14].copy_from_slice(&1u16.to_le_bytes());
    header[14..16].copy_from_slice(&32u16.to_le_bytes());
    for (target, pixel) in data[size_of::<BITMAPV5HEADER>()..]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(pixels.pixels())
    {
        let [r, g, b, a] = pixel.0;
        *target = [b, g, r, a];
    }
    data
}

#[cfg(target_os = "macos")]
pub(super) fn to_tiff(pixels: &RgbaImage) -> Option<Vec<u8>> {
    use image::codecs::tiff::TiffEncoder;

    let mut bytes = Cursor::new(Vec::new());
    TiffEncoder::new(&mut bytes)
        .write_image(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(bytes.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn png_validates_and_preserves_transparency_with_bounded_dimensions() {
        let pixels = RgbaImage::from_pixel(2, 3, Rgba([40, 80, 120, 63]));
        let png = to_png(&pixels).unwrap();
        assert_eq!(decode(&png, ImageFormat::Png).unwrap(), pixels);
        assert!(decode(&png[..20], ImageFormat::Png).is_none());
        let too_wide = RgbaImage::new(MAX_SIDE + 1, 1);
        assert!(decode(&to_png(&too_wide).unwrap(), ImageFormat::Png).is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_dib_preserves_orientation_color_and_alpha() {
        let pixels = RgbaImage::from_fn(2, 2, |x, y| Rgba([100 * x as u8, 80 * y as u8, 150, 63]));
        let png = from_dib(&to_dib(&pixels)).unwrap();
        assert_eq!(decode(&png, ImageFormat::Png).unwrap(), pixels);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_tiff_preserves_dimensions_color_and_alpha() {
        let pixels = RgbaImage::from_fn(2, 2, |x, y| Rgba([100 * x as u8, 80 * y as u8, 150, 63]));
        let tiff = to_tiff(&pixels).unwrap();
        let png = normalize(&tiff, ImageFormat::Tiff).unwrap();
        assert_eq!(decode(&png, ImageFormat::Png).unwrap(), pixels);
    }
}
