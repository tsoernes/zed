use anyhow::Result;
use qrcode::QrCode;

/// Generate a QR code as PNG bytes
pub fn generate_qr_code(data: &str) -> Result<Vec<u8>> {
    // For now, return SVG as bytes
    // TODO: Add proper PNG rendering with image crate
    let svg = generate_qr_code_svg(data)?;
    Ok(svg.into_bytes())
}

/// Generate a QR code as SVG string
pub fn generate_qr_code_svg(data: &str) -> Result<String> {
    let code = QrCode::new(data.as_bytes())?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(200, 200)
        .max_dimensions(400, 400)
        .build();

    Ok(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qr_code_generation() {
        let result = generate_qr_code("ws://192.168.1.100:8080?token=test123");
        assert!(result.is_ok());
        let data = result.unwrap();
        assert!(!data.is_empty());
    }

    #[test]
    fn test_qr_code_svg_generation() {
        let result = generate_qr_code_svg("ws://192.168.1.100:8080?token=test123");
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
    }
}
