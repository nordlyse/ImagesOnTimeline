use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, NaiveDateTime, Utc};

#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub id: i64,
    pub path: PathBuf,
    pub captured_at: DateTime<Utc>,
    pub name: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub place: Option<String>,
}

/// A picture already stored for this path. The file is not opened again.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredPicture {
    pub captured_at: DateTime<Utc>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub place: Option<String>,
}

pub fn is_image(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "heic" | "heif" | "avif" | "tif" | "tiff"
    )
}

pub fn is_video(path: &Path) -> bool {
    matches!(extension(path).as_str(), "mp4" | "m4v" | "mov" | "webm")
}

pub fn scan_folder(folder: &Path, known: &HashMap<String, StoredPicture>) -> Vec<Picture> {
    eprintln!("scanning {}", folder.display());
    let mut pictures = Vec::new();
    let mut folders = 0usize;
    let mut reused = 0usize;
    let mut pending = vec![folder.to_path_buf()];
    while let Some(dir) = pending.pop() {
        folders += 1;
        if folders % 100 == 0 {
            eprintln!("scanned {folders} folders, {} pictures", pictures.len());
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.starts_with('.') || skip_directory(&name) {
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
            if !is_image(&path) && !is_video(&path) {
                continue;
            }
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let stored = path.to_str().and_then(|key| known.get(key));
            let (captured_at, latitude, longitude, place) = if let Some(stored) = stored {
                reused += 1;
                (
                    stored.captured_at,
                    stored.latitude,
                    stored.longitude,
                    stored.place.clone(),
                )
            } else {
                let meta = picture_meta(&path);
                (
                    meta.captured_at.unwrap_or_else(|| DateTime::<Utc>::from(modified)),
                    meta.latitude,
                    meta.longitude,
                    meta.place,
                )
            };
            pictures.push(Picture {
                id: 0,
                path,
                captured_at,
                name,
                latitude,
                longitude,
                place,
            });
            if pictures.len() % 200 == 0 {
                eprintln!("scanned {folders} folders, {} pictures", pictures.len());
            }
        }
    }
    eprintln!(
        "scanned {folders} folders, {} pictures, {reused} already stored",
        pictures.len()
    );
    arrange_pictures(&mut pictures);
    pictures
}

pub fn arrange_pictures(pictures: &mut Vec<Picture>) {
    pictures.sort_by(|left, right| {
        left.captured_at
            .cmp(&right.captured_at)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.path.cmp(&right.path))
    });
    for (offset, picture) in pictures.iter_mut().enumerate() {
        picture.id = i64::try_from(offset).unwrap_or(i64::MAX) + 1;
    }
}

fn skip_directory(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "node_modules"
            | "target"
            | "dist"
            | "build"
            | "vendor"
            | "pods"
            | "deriveddata"
            | "coverage"
            | "__pycache__"
            | "venv"
            | "out"
            | "bin"
            | "obj"
    )
}

#[derive(Debug, Clone, Default)]
struct PictureMeta {
    captured_at: Option<DateTime<Utc>>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    place: Option<String>,
}

fn picture_meta(path: &Path) -> PictureMeta {
    match extension(path).as_str() {
        "jpg" | "jpeg" => meta_from_bytes(&read_prefix(path, 262_144).unwrap_or_default(), true),
        "png" | "tif" | "tiff" => meta_from_bytes(&read_prefix(path, 262_144).unwrap_or_default(), false),
        "heic" | "heif" | "avif" => bounded_meta(path),
        "mp4" | "m4v" | "mov" => PictureMeta {
            captured_at: quicktime_captured_at(path),
            ..PictureMeta::default()
        },
        _ => PictureMeta::default(),
    }
}

fn meta_from_bytes(bytes: &[u8], jpeg: bool) -> PictureMeta {
    let tiff = if jpeg {
        jpeg_exif(bytes).map(read_tiff).unwrap_or_default()
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        png_exif(bytes).map(read_tiff).unwrap_or_default()
    } else {
        read_tiff(bytes)
    };
    PictureMeta {
        captured_at: tiff.captured_at,
        latitude: tiff.latitude,
        longitude: tiff.longitude,
        place: tiff.place.or_else(|| xmp_place(bytes)),
    }
}

/// Phone photos keep the capture record near either end of the file. Reading the
/// whole file stalls a large folder, so only those edges are checked.
fn bounded_meta(path: &Path) -> PictureMeta {
    const WINDOW: u64 = 512 * 1024;
    let Ok(mut file) = File::open(path) else {
        return PictureMeta::default();
    };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
        return PictureMeta::default();
    };
    if length == 0 {
        return PictureMeta::default();
    }
    let head_len = usize::try_from(length.min(WINDOW)).unwrap_or(0);
    let mut head = vec![0; head_len];
    if file.read_exact(&mut head).is_err() {
        return PictureMeta::default();
    }
    let head_meta = window_meta(&head);
    if length <= WINDOW {
        return head_meta;
    }
    let tail_len = usize::try_from(WINDOW.min(length.saturating_sub(WINDOW))).unwrap_or(0);
    if file.seek(SeekFrom::End(-(tail_len as i64))).is_err() {
        return head_meta;
    }
    let mut tail = vec![0; tail_len];
    if file.read_exact(&mut tail).is_err() {
        return head_meta;
    }
    merge_meta(head_meta, window_meta(&tail))
}

fn window_meta(bytes: &[u8]) -> PictureMeta {
    let mut meta = if let Some(tiff) = embedded_exif(bytes) {
        let fields = read_tiff(tiff);
        PictureMeta {
            captured_at: fields.captured_at.or_else(|| find_exif_date(bytes)),
            latitude: fields.latitude,
            longitude: fields.longitude,
            place: fields.place.or_else(|| xmp_place(bytes)),
        }
    } else {
        PictureMeta {
            captured_at: find_exif_date(bytes),
            place: xmp_place(bytes),
            ..PictureMeta::default()
        }
    };
    if meta.place.is_none() {
        meta.place = xmp_place(bytes);
    }
    meta
}

fn merge_meta(first: PictureMeta, second: PictureMeta) -> PictureMeta {
    let (latitude, longitude) = match (first.latitude, first.longitude) {
        (Some(latitude), Some(longitude)) => (Some(latitude), Some(longitude)),
        _ => (second.latitude, second.longitude),
    };
    PictureMeta {
        captured_at: first.captured_at.or(second.captured_at),
        latitude,
        longitude,
        place: first.place.or(second.place),
    }
}

fn embedded_exif(bytes: &[u8]) -> Option<&[u8]> {
    let mut index = 0;
    while index + 14 <= bytes.len() {
        if bytes[index..].starts_with(b"Exif\0\0") {
            let tiff = &bytes[index + 6..];
            if tiff.len() >= 8 && (tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*")) {
                return Some(tiff);
            }
        }
        index += 1;
    }
    None
}

fn find_exif_date(bytes: &[u8]) -> Option<DateTime<Utc>> {
    if bytes.len() < 19 {
        return None;
    }
    let mut index = 0;
    while index + 19 <= bytes.len() {
        if is_exif_date(&bytes[index..index + 19]) {
            let text = std::str::from_utf8(&bytes[index..index + 19]).ok()?;
            if let Some(when) = parse_exif_text(text) {
                return Some(when);
            }
        }
        index += 1;
    }
    None
}

fn is_exif_date(bytes: &[u8]) -> bool {
    bytes.len() == 19
        && bytes[4] == b':'
        && bytes[7] == b':'
        && bytes[10] == b' '
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[..4].iter().chain(bytes[5..7].iter()).chain(bytes[8..10].iter()).all(u8::is_ascii_digit)
        && bytes[11..13].iter().chain(bytes[14..16].iter()).chain(bytes[17..19].iter()).all(u8::is_ascii_digit)
}

fn read_prefix(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = vec![0; limit];
    let read = reader.read(&mut buffer).ok()?;
    buffer.truncate(read);
    Some(buffer)
}

fn jpeg_exif(bytes: &[u8]) -> Option<&[u8]> {
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
                if tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*") {
                    return Some(tiff);
                }
            }
        }
        index += length;
    }
    None
}

fn png_exif(bytes: &[u8]) -> Option<&[u8]> {
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
            return Some(&bytes[start..end]);
        }
        if kind == b"IEND" {
            return None;
        }
        index = end + 4;
    }
    None
}

#[derive(Debug, Clone, Default)]
struct TiffFields {
    captured_at: Option<DateTime<Utc>>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    place: Option<String>,
}

#[cfg(test)]
fn tiff_date(bytes: &[u8]) -> Option<DateTime<Utc>> {
    read_tiff(bytes).captured_at
}

fn read_tiff(bytes: &[u8]) -> TiffFields {
    let mut fields = TiffFields::default();
    if bytes.len() < 8 {
        return fields;
    }
    let little = match &bytes[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return fields,
    };
    if read_u16(bytes, 2, little) != Some(42) {
        return fields;
    }
    let Some(start) = read_u32(bytes, 4, little) else {
        return fields;
    };
    let mut offset = start as usize;
    let mut original = None;
    let mut modified = None;
    let mut gps_offset = None;
    let mut follow_exif = true;
    for _ in 0..6 {
        if offset.checked_add(2).map(|end| end > bytes.len()).unwrap_or(true) {
            break;
        }
        let Some(count) = read_u16(bytes, offset, little) else {
            break;
        };
        offset += 2;
        let mut exif_offset = None;
        for _ in 0..count {
            if offset.checked_add(12).map(|end| end > bytes.len()).unwrap_or(true) {
                fields.captured_at = original.or(modified);
                return fields;
            }
            let Some(tag) = read_u16(bytes, offset, little) else {
                break;
            };
            let Some(kind) = read_u16(bytes, offset + 2, little) else {
                break;
            };
            let Some(count) = read_u32(bytes, offset + 4, little) else {
                break;
            };
            let Some(field) = bytes.get(offset + 8..offset + 12) else {
                break;
            };
            if follow_exif && tag == 0x8769 && kind == 4 {
                exif_offset = read_u32(field, 0, little).map(|value| value as usize);
            }
            if follow_exif && tag == 0x8825 && kind == 4 {
                gps_offset = read_u32(field, 0, little).map(|value| value as usize);
            }
            if kind == 2 && (tag == 0x9003 || tag == 0x0132) {
                if let Some(text) = ascii_value(bytes, field, count as usize, little) {
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
        if let Some(next) = exif_offset.filter(|_| original.is_none()) {
            offset = next;
            follow_exif = false;
            continue;
        }
        break;
    }
    fields.captured_at = original.or(modified);
    if let Some(gps) = gps_offset {
        apply_gps(&mut fields, bytes, gps, little);
    }
    fields
}

fn apply_gps(fields: &mut TiffFields, bytes: &[u8], offset: usize, little: bool) {
    let Some(count) = read_u16(bytes, offset, little) else {
        return;
    };
    let mut latitude = None;
    let mut latitude_ref = None;
    let mut longitude = None;
    let mut longitude_ref = None;
    let mut place = None;
    let mut cursor = offset + 2;
    for _ in 0..count {
        if cursor + 12 > bytes.len() {
            break;
        }
        let Some(tag) = read_u16(bytes, cursor, little) else {
            break;
        };
        let Some(kind) = read_u16(bytes, cursor + 2, little) else {
            break;
        };
        let Some(count) = read_u32(bytes, cursor + 4, little) else {
            break;
        };
        let Some(field) = bytes.get(cursor + 8..cursor + 12) else {
            break;
        };
        match tag {
            1 => latitude_ref = ascii_value(bytes, field, count as usize, little),
            2 => latitude = read_dms(bytes, field, count, kind, little),
            3 => longitude_ref = ascii_value(bytes, field, count as usize, little),
            4 => longitude = read_dms(bytes, field, count, kind, little),
            0x1C => place = gps_text(bytes, field, count as usize, kind, little),
            _ => {}
        }
        cursor += 12;
    }
    if let (Some(latitude), Some(longitude)) = (latitude, longitude) {
        let latitude = signed_coordinate(latitude, latitude_ref.as_deref(), "S");
        let longitude = signed_coordinate(longitude, longitude_ref.as_deref(), "W");
        if latitude.abs() <= 90.0 && longitude.abs() <= 180.0 {
            fields.latitude = Some(latitude);
            fields.longitude = Some(longitude);
        }
    }
    if fields.place.is_none() {
        fields.place = place.and_then(|text| clean_place(&text));
    }
}

fn signed_coordinate(value: f64, reference: Option<&str>, negative: &str) -> f64 {
    if reference.is_some_and(|reference| reference.eq_ignore_ascii_case(negative)) {
        -value.abs()
    } else {
        value.abs()
    }
}

fn read_dms(bytes: &[u8], field: &[u8], count: u32, kind: u16, little: bool) -> Option<f64> {
    if kind != 5 || count != 3 {
        return None;
    }
    let start = read_u32(field, 0, little)? as usize;
    let degrees = read_rational(bytes, start, little)?;
    let minutes = read_rational(bytes, start + 8, little)?;
    let seconds = read_rational(bytes, start + 16, little)?;
    Some(degrees + minutes / 60.0 + seconds / 3600.0)
}

fn read_rational(bytes: &[u8], offset: usize, little: bool) -> Option<f64> {
    let numerator = read_u32(bytes, offset, little)?;
    let denominator = read_u32(bytes, offset + 4, little)?;
    if denominator == 0 {
        return None;
    }
    Some(f64::from(numerator) / f64::from(denominator))
}

fn gps_text(bytes: &[u8], field: &[u8], count: usize, kind: u16, little: bool) -> Option<String> {
    if kind != 2 && kind != 7 {
        return None;
    }
    let text = ascii_value(bytes, field, count, little)?;
    let bytes = text.as_bytes();
    let text = bytes
        .strip_prefix(b"ASCII\0\0\0")
        .map(|rest| String::from_utf8_lossy(rest).into_owned())
        .unwrap_or(text);
    Some(text)
}

fn xmp_place(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let keys = [
        "Iptc4xmpCore:Location",
        "photoshop:City",
        "photoshop:State",
        "photoshop:Country",
    ];
    let mut parts = Vec::new();
    for key in keys {
        let Some(value) = xmp_value(&text, key) else {
            continue;
        };
        if parts.iter().any(|part: &String| part.eq_ignore_ascii_case(&value)) {
            continue;
        }
        parts.push(value);
    }
    if parts.is_empty() {
        None
    } else {
        clean_place(&parts.join(", "))
    }
}

fn xmp_value(text: &str, key: &str) -> Option<String> {
    let element = format!("<{key}>");
    if let Some(start) = text.find(&element) {
        let rest = &text[start + element.len()..];
        if let Some(end) = rest.find('<') {
            return clean_place(&decode_xml(rest[..end].trim()));
        }
    }
    let attribute = format!("{key}=\"");
    let start = text.find(&attribute)? + attribute.len();
    let rest = text.get(start..)?;
    let end = rest.find('"')?;
    clean_place(&decode_xml(rest[..end].trim()))
}

fn decode_xml(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn clean_place(value: &str) -> Option<String> {
    let text: String = value.chars().filter(|ch| !ch.is_control()).take(160).collect();
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn ascii_value(bytes: &[u8], field: &[u8], count: usize, little: bool) -> Option<String> {
    if count == 0 || count > 160 {
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

fn quicktime_captured_at(path: &Path) -> Option<DateTime<Utc>> {
    const WINDOW: u64 = 1024 * 1024;
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length == 0 {
        return None;
    }
    let head_len = usize::try_from(length.min(WINDOW)).unwrap_or(0);
    let mut head = vec![0; head_len];
    file.read_exact(&mut head).ok()?;
    if let Some(unix) = quicktime_unix(&head) {
        return DateTime::from_timestamp(unix, 0);
    }
    if length <= WINDOW {
        return None;
    }
    let tail_len = usize::try_from(WINDOW.min(length.saturating_sub(WINDOW))).unwrap_or(0);
    if file.seek(SeekFrom::End(-(tail_len as i64))).is_err() {
        return None;
    }
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail).ok()?;
    DateTime::from_timestamp(quicktime_unix(&tail)?, 0)
}

fn quicktime_unix(bytes: &[u8]) -> Option<i64> {
    const MAC_EPOCH: i64 = 2_082_844_800;
    let mut start = 0;
    while let Some(rel) = bytes[start..].windows(4).position(|window| window == b"mvhd") {
        let at = start + rel;
        let version = *bytes.get(at + 4)?;
        let qt = if version == 0 {
            u32::from_be_bytes(bytes.get(at + 8..at + 12)?.try_into().ok()?) as i64
        } else if version == 1 {
            i64::try_from(u64::from_be_bytes(bytes.get(at + 8..at + 16)?.try_into().ok()?)).ok()?
        } else {
            start = at + 4;
            continue;
        };
        let unix = qt.checked_sub(MAC_EPOCH)?;
        if (0..4_102_444_800).contains(&unix) {
            return Some(unix);
        }
        start = at + 4;
    }
    None
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
    fn reads_an_exif_date_inside_a_byte_window() {
        let mut bytes = vec![0; 40];
        bytes.extend_from_slice(b"2014:03:22 18:04:05");
        let when = find_exif_date(&bytes).unwrap();
        assert_eq!(when.format("%Y-%m-%d %H:%M:%S").to_string(), "2014-03-22 18:04:05");
        assert!(find_exif_date(b"2014-03-22 18:04:05").is_none());
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

        let pictures = scan_folder(&root, &HashMap::new());
        let _ = fs::remove_dir_all(&root);

        assert_eq!(pictures.len(), 2);
        assert_eq!(pictures[0].name, "earlier.png");
        assert_eq!(pictures[1].name, "later.png");
        assert_eq!(pictures[0].id, 1);
        assert!(pictures[0].captured_at < pictures[1].captured_at);
        assert!(pictures[0].latitude.is_none());
        assert!(pictures[0].place.is_none());
    }

    #[test]
    fn reuses_a_stored_file_name_instead_of_the_file_time() {
        let root = std::env::temp_dir().join(format!("timeline-known-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("kept.png");
        fs::write(&path, b"kept").unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(9_000, 0)).unwrap();
        let stored_at = DateTime::<Utc>::from(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000));
        let mut known = HashMap::new();
        known.insert(
            path.to_str().unwrap().to_string(),
            StoredPicture {
                captured_at: stored_at,
                latitude: Some(41.0),
                longitude: Some(29.0),
                place: Some("Istanbul".to_string()),
            },
        );

        let pictures = scan_folder(&root, &known);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(pictures.len(), 1);
        assert_eq!(pictures[0].name, "kept.png");
        assert_eq!(pictures[0].captured_at, stored_at);
        assert_eq!(pictures[0].latitude, Some(41.0));
        assert_eq!(pictures[0].place.as_deref(), Some("Istanbul"));
    }

    #[test]
    fn lays_a_video_out_by_its_quicktime_date() {
        let root = std::env::temp_dir().join(format!("timeline-video-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let unix = 1_400_000_000i64;
        let qt = (unix + 2_082_844_800) as u32;
        let mut clip = vec![0u8; 8];
        clip.extend_from_slice(&32u32.to_be_bytes());
        clip.extend_from_slice(b"mvhd");
        clip.push(0);
        clip.extend_from_slice(&[0, 0, 0]);
        clip.extend_from_slice(&qt.to_be_bytes());
        fs::write(root.join("clip.mp4"), &clip).unwrap();
        fs::write(root.join("still.png"), b"still").unwrap();
        filetime::set_file_mtime(root.join("clip.mp4"), filetime::FileTime::from_unix_time(5_000, 0)).unwrap();
        filetime::set_file_mtime(root.join("still.png"), filetime::FileTime::from_unix_time(1_000, 0)).unwrap();
        let pictures = scan_folder(&root, &HashMap::new());
        let _ = fs::remove_dir_all(&root);
        assert_eq!(pictures.len(), 2);
        assert_eq!(pictures[1].name, "clip.mp4");
        assert!(is_video(Path::new("clip.mp4")));
        assert_eq!(pictures[1].captured_at.timestamp(), unix);
        assert!(pictures[0].captured_at < pictures[1].captured_at);
    }

    #[test]
    fn lays_a_webm_out_by_its_file_time() {
        let root = std::env::temp_dir().join(format!("timeline-webm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("clip.webm"), b"webm").unwrap();
        fs::write(root.join("still.png"), b"still").unwrap();
        filetime::set_file_mtime(root.join("clip.webm"), filetime::FileTime::from_unix_time(8_000, 0)).unwrap();
        filetime::set_file_mtime(root.join("still.png"), filetime::FileTime::from_unix_time(1_000, 0)).unwrap();
        let pictures = scan_folder(&root, &HashMap::new());
        let _ = fs::remove_dir_all(&root);
        assert_eq!(pictures.len(), 2);
        assert_eq!(pictures[1].name, "clip.webm");
        assert!(is_video(Path::new("clip.webm")));
        assert!(!is_video(Path::new("still.png")));
        assert_eq!(pictures[1].captured_at.timestamp(), 8_000);
    }

    #[test]
    fn reads_a_64_bit_quicktime_date() {
        let root = std::env::temp_dir().join(format!("timeline-mov-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let unix = 1_500_000_000i64;
        let qt = (unix + 2_082_844_800) as u64;
        let mut clip = vec![0u8; 4];
        clip.extend_from_slice(&48u32.to_be_bytes());
        clip.extend_from_slice(b"mvhd");
        clip.push(1);
        clip.extend_from_slice(&[0, 0, 0]);
        clip.extend_from_slice(&qt.to_be_bytes());
        fs::write(root.join("clip.mov"), &clip).unwrap();
        let pictures = scan_folder(&root, &HashMap::new());
        let _ = fs::remove_dir_all(&root);
        assert_eq!(pictures.len(), 1);
        assert_eq!(pictures[0].name, "clip.mov");
        assert!(is_video(Path::new("clip.MOV")));
        assert_eq!(pictures[0].captured_at.timestamp(), unix);
    }

    #[test]
    fn reads_coordinates_from_a_gps_directory() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II");
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&0x8825u16.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&26u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        push_ascii_entry(&mut bytes, 1, b"N\0");
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&5u16.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&80u32.to_le_bytes());
        push_ascii_entry(&mut bytes, 3, b"W\0");
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&5u16.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&104u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        push_rational(&mut bytes, 41, 1);
        push_rational(&mut bytes, 30, 1);
        push_rational(&mut bytes, 0, 1);
        push_rational(&mut bytes, 29, 1);
        push_rational(&mut bytes, 0, 1);
        push_rational(&mut bytes, 0, 1);

        let fields = read_tiff(&bytes);
        assert!((fields.latitude.unwrap() - 41.5).abs() < 0.0001);
        assert!((fields.longitude.unwrap() + 29.0).abs() < 0.0001);
    }

    fn push_ascii_entry(bytes: &mut Vec<u8>, tag: u16, text: &[u8]) {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&(text.len() as u32).to_le_bytes());
        let mut value = [0u8; 4];
        value[..text.len()].copy_from_slice(text);
        bytes.extend_from_slice(&value);
    }

    fn push_rational(bytes: &mut Vec<u8>, numerator: u32, denominator: u32) {
        bytes.extend_from_slice(&numerator.to_le_bytes());
        bytes.extend_from_slice(&denominator.to_le_bytes());
    }

    #[test]
    fn reads_a_place_from_xmp() {
        let bytes = br#"<photoshop:City>Istanbul</photoshop:City><photoshop:Country>Turkey</photoshop:Country>"#;
        assert_eq!(xmp_place(bytes).as_deref(), Some("Istanbul, Turkey"));
    }
}
