use anyhow::Result;
use image::{ImageBuffer, ImageFormat, Luma};
use qrcode::QrCode;
use qrcode::render::svg;

/// Generate a QR code as PNG bytes.
///
/// This is intended for UI display where an `img` element is more reliable than SVG rendering.
pub fn generate_qr_code(data: &str) -> Result<Vec<u8>> {
    let code = QrCode::new(data.as_bytes())?;

    // Render into an image buffer (monochrome). Use a modest module scale so it stays readable
    // while keeping the PNG reasonably sized.
    let image: ImageBuffer<Luma<u8>, Vec<u8>> = code
        .render::<Luma<u8>>()
        .min_dimensions(240, 240)
        .max_dimensions(480, 480)
        .quiet_zone(true)
        .build();

    let mut png_bytes = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut png_bytes), ImageFormat::Png)?;

    Ok(png_bytes)
}

/// Generate a QR code as SVG string.
pub fn generate_qr_code_svg(data: &str) -> Result<String> {
    let code = QrCode::new(data.as_bytes())?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(200, 200)
        .max_dimensions(400, 400)
        .build();

    Ok(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qr_code_generation_png() {
        let result = generate_qr_code("ws://192.168.1.100:8080?token=test123");
        assert!(result.is_ok());
        let data = result.unwrap();
        assert!(!data.is_empty());
        // PNG signature
        assert!(data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
    }

    #[test]
    fn test_qr_code_svg_generation() {
        let result = generate_qr_code_svg("ws://192.168.1.100:8080?token=test123");
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
    }
}
