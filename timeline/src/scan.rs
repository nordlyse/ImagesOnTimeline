use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, NaiveDateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub id: i64,
    pub path: PathBuf,
    pub captured_at: DateTime<Utc>,
    pub name: String,
}

pub fn is_image(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "heic" | "heif" | "avif" | "tif" | "tiff"
    )
}

pub fn scan_folder(folder: &Path) -> Vec<Picture> {
    let mut pictures = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !is_image(&path) {
                continue;
            }
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let captured_at = metadata_time(&path).unwrap_or_else(|| DateTime::<Utc>::from(modified));
            pictures.push(Picture {
                id: 0,
                path,
                captured_at,
                name,
            });
        }
    }
    pictures.sort_by(|left, right| {
        left.captured_at
            .cmp(&right.captured_at)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.path.cmp(&right.path))
    });
    for (offset, picture) in pictures.iter_mut().enumerate() {
        picture.id = i64::try_from(offset).unwrap_or(i64::MAX) + 1;
    }
    pictures
}

fn metadata_time(path: &Path) -> Option<DateTime<Utc>> {
    match extension(path).as_str() {
        "jpg" | "jpeg" => jpeg_date(&read_prefix(path, 1_048_576)?),
        "png" | "tif" | "tiff" => tiff_or_png_date(&read_prefix(path, 1_048_576)?),
        "heic" | "heif" | "avif" => heic_date(&fs::read(path).ok()?),
        _ => None,
    }
}

fn tiff_or_png_date(bytes: &[u8]) -> Option<DateTime<Utc>> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        png_date(bytes)
    } else {
        tiff_date(bytes)
    }
}

fn read_prefix(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = vec![0; limit];
    let read = reader.read(&mut buffer).ok()?;
    buffer.truncate(read);
    Some(buffer)
}

fn jpeg_date(bytes: &[u8]) -> Option<DateTime<Utc>> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut index = 2;
    while index + 4 < bytes.len() {
        if bytes[index] != 0xFF {
            return None;
        }
        while index < bytes.len() && bytes[index] == 0xFF {
            index += 1;
        }
        if index >= bytes.len() {
            return None;
        }
        let marker = bytes[index];
        index += 1;
        if marker == 0xD8 || marker == 0xD9 || marker == 0xDA || (0xD0..=0xD7).contains(&marker) {
            if marker == 0xD9 || marker == 0xDA {
                return None;
            }
            continue;
        }
        if index + 2 > bytes.len() {
            return None;
        }
        let length = u16::from_be_bytes([bytes[index], bytes[index + 1]]) as usize;
        if length < 2 || index + length > bytes.len() {
            return None;
        }
        if marker == 0xE1 {
            let payload = &bytes[index + 2..index + length];
            if let Some(tiff) = payload.strip_prefix(b"Exif\0\0") {
                if let Some(when) = tiff_date(tiff) {
                    return Some(when);
                }
            }
        }
        index += length;
    }
    None
}

fn png_date(bytes: &[u8]) -> Option<DateTime<Utc>> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut index = 8;
    while index + 12 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[index..index + 4].try_into().ok()?) as usize;
        let kind = &bytes[index + 4..index + 8];
        let start = index + 8;
        let end = start.checked_add(length)?;
        if end + 4 > bytes.len() {
            return None;
        }
        if kind == b"eXIf" {
            return tiff_date(&bytes[start..end]);
        }
        if kind == b"IEND" {
            return None;
        }
        index = end + 4;
    }
    None
}

fn heic_date(bytes: &[u8]) -> Option<DateTime<Utc>> {
    let context = heic_rs::context::Context::open(bytes).ok()?;
    let tiff = context.exif(context.meta.primary).ok()??;
    tiff_date(tiff)
}

fn tiff_date(bytes: &[u8]) -> Option<DateTime<Utc>> {
    if bytes.len() < 8 {
        return None;
    }
    let little = match &bytes[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    if read_u16(bytes, 2, little)? != 42 {
        return None;
    }
    let mut offset = read_u32(bytes, 4, little)? as usize;
    let mut original = None;
    let mut modified = None;
    let mut follow_exif = true;
    for _ in 0..6 {
        if offset.checked_add(2)? > bytes.len() {
            break;
        }
        let count = read_u16(bytes, offset, little)? as usize;
        offset += 2;
        let mut exif_offset = None;
        for _ in 0..count {
            if offset.checked_add(12)? > bytes.len() {
                return original.or(modified);
            }
            let tag = read_u16(bytes, offset, little)?;
            let kind = read_u16(bytes, offset + 2, little)?;
            let count = read_u32(bytes, offset + 4, little)? as usize;
            let field = bytes.get(offset + 8..offset + 12)?;
            if follow_exif && tag == 0x8769 && kind == 4 {
                exif_offset = Some(read_u32(field, 0, little)? as usize);
            }
            if kind == 2 && (tag == 0x9003 || tag == 0x0132) {
                if let Some(text) = ascii_value(bytes, field, count, little) {
                    let when = parse_exif_text(&text);
                    if tag == 0x9003 {
                        original = original.or(when);
                    } else {
                        modified = modified.or(when);
                    }
                }
            }
            offset += 12;
        }
        if original.is_some() {
            return original;
        }
        if let Some(next) = exif_offset {
            offset = next;
            follow_exif = false;
            continue;
        }
        break;
    }
    original.or(modified)
}

fn ascii_value(bytes: &[u8], field: &[u8], count: usize, little: bool) -> Option<String> {
    if count == 0 || count > 64 {
        return None;
    }
    let raw = if count <= 4 {
        field.get(..count)?
    } else {
        let start = read_u32(field, 0, little)? as usize;
        bytes.get(start..start.checked_add(count)?)?
    };
    let text = raw.split(|byte| *byte == 0).next().unwrap_or(raw);
    Some(String::from_utf8_lossy(text).into_owned())
}

fn read_u16(bytes: &[u8], offset: usize, little: bool) -> Option<u16> {
    let chunk = bytes.get(offset..offset + 2)?;
    Some(if little {
        u16::from_le_bytes([chunk[0], chunk[1]])
    } else {
        u16::from_be_bytes([chunk[0], chunk[1]])
    })
}

fn read_u32(bytes: &[u8], offset: usize, little: bool) -> Option<u32> {
    let chunk = bytes.get(offset..offset + 4)?;
    Some(if little {
        u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
    } else {
        u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
    })
}

fn parse_exif_text(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(when) = DateTime::parse_from_rfc3339(text) {
        return Some(when.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(text, "%Y:%m:%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S"))
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S"))
        .ok()
        .map(|when| when.and_utc())
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_common_exif_text_dates() {
        let when = parse_exif_text("2014:03:22 18:04:05").unwrap();
        assert_eq!(when.format("%Y-%m-%d %H:%M:%S").to_string(), "2014-03-22 18:04:05");
        assert!(parse_exif_text("not a date").is_none());
    }

    #[test]
    fn reads_a_tiff_capture_time() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II");
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&0x9003u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&20u32.to_le_bytes());
        bytes.extend_from_slice(&26u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"2014:03:22 18:04:05\0");
        let when = tiff_date(&bytes).unwrap();
        assert_eq!(when.format("%Y-%m-%d %H:%M:%S").to_string(), "2014-03-22 18:04:05");
    }

    #[test]
    fn lays_nested_pictures_out_by_file_time() {
        let root = std::env::temp_dir().join(format!("timeline-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("trip")).unwrap();
        fs::write(root.join("notes.txt"), b"skip").unwrap();
        fs::write(root.join("later.png"), b"later").unwrap();
        fs::write(root.join("trip/earlier.png"), b"earlier").unwrap();
        filetime::set_file_mtime(
            root.join("trip/earlier.png"),
            filetime::FileTime::from_unix_time(1_000, 0),
        )
        .unwrap();
        filetime::set_file_mtime(root.join("later.png"), filetime::FileTime::from_unix_time(5_000, 0))
            .unwrap();

        let pictures = scan_folder(&root);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(pictures.len(), 2);
        assert_eq!(pictures[0].name, "earlier.png");
        assert_eq!(pictures[1].name, "later.png");
        assert_eq!(pictures[0].id, 1);
        assert!(pictures[0].captured_at < pictures[1].captured_at);
    }
}
