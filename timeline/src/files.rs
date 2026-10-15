use std::fs;
use std::path::Path;

pub fn content_type_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "avif" => "image/avif",
        "tif" | "tiff" => "image/tiff",
        _ => "application/octet-stream",
    }
}

pub fn read_picture(root: &Path, stored: &Path) -> Option<(Vec<u8>, &'static str)> {
    let root = root.canonicalize().ok()?;
    let file = stored.canonicalize().ok()?;
    if !file.is_file() || !file.starts_with(&root) {
        return None;
    }
    let bytes = fs::read(&file).ok()?;
    Some(present_picture(&file, bytes))
}

/// Browsers other than Safari do not draw HEIC, so a phone photo is sent as JPEG.
pub fn present_picture(path: &Path, bytes: Vec<u8>) -> (Vec<u8>, &'static str) {
    let content_type = content_type_for(path);
    if content_type == "image/heic" || content_type == "image/heif" {
        if let Some(jpeg) = jpeg_from_heic(&bytes) {
            return (jpeg, "image/jpeg");
        }
    }
    (bytes, content_type)
}

fn jpeg_from_heic(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = heic_rs::decode(bytes, &heic_rs::DecodeOptions::default()).ok()?;
    if image.layout != heic_rs::PixelLayout::Rgb8 {
        return None;
    }
    let width = u16::try_from(image.width).ok()?;
    let height = u16::try_from(image.height).ok()?;
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 85)
        .encode(&image.data, width, height, jpeg_encoder::ColorType::Rgb)
        .ok()?;
    Some(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_picture_types() {
        assert_eq!(content_type_for(Path::new("a.JPG")), "image/jpeg");
        assert_eq!(content_type_for(Path::new("phone.HEIC")), "image/heic");
        assert_eq!(content_type_for(Path::new("mark.png")), "image/png");
    }

    #[test]
    fn leaves_a_jpeg_unchanged() {
        let bytes = b"jpeg-bytes".to_vec();
        let (out, kind) = present_picture(Path::new("a.jpg"), bytes.clone());
        assert_eq!(kind, "image/jpeg");
        assert_eq!(out, bytes);
    }

    #[test]
    fn reads_only_files_inside_the_library_root() {
        let root = std::env::temp_dir().join(format!("timeline-root-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("ok.jpg"), b"jpeg").unwrap();

        assert!(read_picture(&root, &root.join("ok.jpg")).is_some());
        assert!(read_picture(&root, Path::new("/etc/passwd")).is_none());

        let _ = fs::remove_dir_all(&root);
    }
}
