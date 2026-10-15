use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::scan::{Picture, StoredPicture};

const PREPARE_PICTURES: &str = "\
CREATE TABLE IF NOT EXISTS picture (
  folder STRING,
  file_path STRING,
  file_name STRING,
  captured_at TIMESTAMP,
  latitude DOUBLE,
  longitude DOUBLE,
  place STRING,
  ts TIMESTAMP TIME INDEX,
  PRIMARY KEY(folder, file_path)
)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    host: String,
    port: u16,
    user: String,
    database: String,
}

pub struct Session {
    stream: TcpStream,
}

impl Catalog {
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url
            .strip_prefix("postgres://")
            .or_else(|| url.strip_prefix("postgresql://"))?;
        let (user, host_path) = match rest.split_once('@') {
            Some((auth, host_path)) => {
                let user = auth.split_once(':').map(|(user, _)| user).unwrap_or(auth);
                (user, host_path)
            }
            None => ("", rest),
        };
        let (host_port, database) = host_path.split_once('/')?;
        let database = database.split('?').next().unwrap_or(database);
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (host_port, 5432),
        };
        if user.is_empty() || host.is_empty() || database.is_empty() {
            return None;
        }
        Some(Self {
            host: host.to_string(),
            port,
            user: user.to_string(),
            database: database.to_string(),
        })
    }

    pub fn label(&self) -> String {
        format!("{}:{}/{}", self.host, self.port, self.database)
    }

    pub fn open(&self) -> Result<Session, String> {
        let stream = TcpStream::connect((self.host.as_str(), self.port)).map_err(|err| err.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .map_err(|err| err.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(20)))
            .map_err(|err| err.to_string())?;
        let mut session = Session { stream };
        session.startup(&self.user, &self.database)?;
        Ok(session)
    }
}

impl Session {
    pub fn prepare(&mut self) -> Result<(), String> {
        self.query(PREPARE_PICTURES).map(|_| ())
    }

    pub fn load_folder(&mut self, folder: &str) -> Result<HashMap<String, StoredPicture>, String> {
        let sql = format!(
            "SELECT file_path, file_name, captured_at, latitude, longitude, place FROM picture WHERE folder = {} ORDER BY ts DESC",
            sql_string(folder)
        );
        let rows = self.query(&sql)?;
        let mut known = HashMap::new();
        for row in rows {
            let Some(path) = row.first().and_then(|value| value.clone()).filter(|value| !value.is_empty()) else {
                continue;
            };
            let Some(captured_at) = row.get(2).and_then(|value| value.as_deref()).and_then(parse_timestamp) else {
                continue;
            };
            if known.contains_key(&path) {
                continue;
            }
            known.insert(
                path,
                StoredPicture {
                    captured_at,
                    latitude: row.get(3).and_then(|value| value.as_deref()).and_then(parse_number),
                    longitude: row.get(4).and_then(|value| value.as_deref()).and_then(parse_number),
                    place: row.get(5).and_then(|value| value.clone()).filter(|value| !value.is_empty()),
                },
            );
        }
        Ok(known)
    }

    pub fn save_pictures(&mut self, folder: &str, pictures: &[&Picture]) -> Result<(), String> {
        for chunk in pictures.chunks(40) {
            let mut values = String::new();
            let recorded_at = Utc::now().format("%Y-%m-%d %H:%M:%S");
            for picture in chunk {
                let Some(path) = picture.path.to_str() else {
                    continue;
                };
                if !values.is_empty() {
                    values.push(',');
                }
                values.push('(');
                values.push_str(&sql_string(folder));
                values.push(',');
                values.push_str(&sql_string(path));
                values.push(',');
                values.push_str(&sql_string(&picture.name));
                values.push_str(",'");
                values.push_str(&picture.captured_at.format("%Y-%m-%d %H:%M:%S").to_string());
                values.push_str("',");
                values.push_str(&sql_number(picture.latitude));
                values.push(',');
                values.push_str(&sql_number(picture.longitude));
                values.push(',');
                values.push_str(&sql_optional(picture.place.as_deref()));
                values.push_str(",'");
                values.push_str(&recorded_at.to_string());
                values.push_str("')");
            }
            if values.is_empty() {
                continue;
            }
            let sql = format!(
                "INSERT INTO picture (folder, file_path, file_name, captured_at, latitude, longitude, place, ts) VALUES {values}"
            );
            self.query(&sql)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn remove_folder(&mut self, folder: &str) -> Result<(), String> {
        self.query(&format!("DELETE FROM picture WHERE folder = {}", sql_string(folder)))
            .map(|_| ())
    }

    fn startup(&mut self, user: &str, database: &str) -> Result<(), String> {
        let mut params = Vec::new();
        params.extend_from_slice(b"user\0");
        params.extend_from_slice(user.as_bytes());
        params.extend_from_slice(b"\0database\0");
        params.extend_from_slice(database.as_bytes());
        params.extend_from_slice(b"\0\0");
        let mut payload = Vec::new();
        payload.extend_from_slice(&196608u32.to_be_bytes());
        payload.extend_from_slice(&params);
        let mut message = (payload.len() as u32 + 4).to_be_bytes().to_vec();
        message.extend_from_slice(&payload);
        self.stream.write_all(&message).map_err(|err| err.to_string())?;
        loop {
            let (tag, body) = read_message(&mut self.stream)?;
            match tag {
                b'R' => {
                    let code = body.get(..4).map(|bytes| {
                        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                    });
                    if code != Some(0) {
                        return Err("the database asked for a password".to_string());
                    }
                }
                b'E' => return Err(error_text(&body)),
                b'Z' => return Ok(()),
                _ => {}
            }
        }
    }

    fn query(&mut self, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
        let mut payload = sql.as_bytes().to_vec();
        payload.push(0);
        let mut message = Vec::with_capacity(payload.len() + 5);
        message.push(b'Q');
        message.extend_from_slice(&(payload.len() as u32 + 4).to_be_bytes());
        message.extend_from_slice(&payload);
        self.stream.write_all(&message).map_err(|err| err.to_string())?;
        let mut rows = Vec::new();
        let mut error = None;
        loop {
            let (tag, body) = read_message(&mut self.stream)?;
            match tag {
                b'D' => rows.push(parse_row(&body)?),
                b'E' => error = Some(error_text(&body)),
                b'Z' => break,
                _ => {}
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(rows),
        }
    }
}

pub fn fresh_pictures<'a>(pictures: &'a [Picture], known: &HashMap<String, StoredPicture>) -> Vec<&'a Picture> {
    pictures
        .iter()
        .filter(|picture| match picture.path.to_str() {
            Some(path) => !known.contains_key(path),
            None => false,
        })
        .collect()
}

fn sql_string(value: &str) -> String {
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\0' {
            continue;
        }
        if ch == '\'' {
            out.push_str("''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn sql_optional(value: Option<&str>) -> String {
    match value {
        Some(value) => sql_string(value),
        None => "NULL".to_string(),
    }
}

fn sql_number(value: Option<f64>) -> String {
    match value {
        Some(value) if value.is_finite() => format!("{value:.6}"),
        _ => "NULL".to_string(),
    }
}

fn parse_number(value: &str) -> Option<f64> {
    let number = value.trim().parse::<f64>().ok()?;
    number.is_finite().then_some(number)
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    let text = value.trim();
    let text = if text.len() >= 19 { &text[..19] } else { text };
    NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|when| when.and_utc())
}

fn parse_row(body: &[u8]) -> Result<Vec<Option<String>>, String> {
    if body.len() < 2 {
        return Err("the database row is short".to_string());
    }
    let count = u16::from_be_bytes([body[0], body[1]]) as usize;
    let mut fields = Vec::with_capacity(count);
    let mut index = 2;
    for _ in 0..count {
        if index + 4 > body.len() {
            return Err("the database field is short".to_string());
        }
        let length = i32::from_be_bytes(body[index..index + 4].try_into().unwrap());
        index += 4;
        if length < 0 {
            fields.push(None);
            continue;
        }
        let length = length as usize;
        let end = index + length;
        if end > body.len() {
            return Err("the database value is short".to_string());
        }
        fields.push(Some(String::from_utf8_lossy(&body[index..end]).into_owned()));
        index = end;
    }
    Ok(fields)
}

fn error_text(body: &[u8]) -> String {
    let mut index = 0;
    let mut message = String::new();
    while index < body.len() {
        let kind = body[index];
        if kind == 0 {
            break;
        }
        index += 1;
        let rest = body.get(index..).unwrap_or_default();
        let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
        if kind == b'M' {
            message = String::from_utf8_lossy(&rest[..end]).into_owned();
        }
        index += end + 1;
    }
    if message.is_empty() {
        "the database rejected the query".to_string()
    } else {
        message
    }
}

fn read_message(stream: &mut TcpStream) -> Result<(u8, Vec<u8>), String> {
    let mut head = [0; 5];
    read_exact(stream, &mut head)?;
    let length = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if length < 4 || length > 8 * 1024 * 1024 {
        return Err("the database message is an unexpected size".to_string());
    }
    let mut body = vec![0; length - 4];
    read_exact(stream, &mut body)?;
    Ok((head[0], body))
}

fn read_exact(stream: &mut TcpStream, buffer: &mut [u8]) -> Result<(), String> {
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => return Err("the database closed the connection".to_string()),
            Ok(read) => filled += read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_a_postgres_address() {
        let catalog = Catalog::parse("postgres://greptime@database:4003/public").unwrap();
        assert_eq!(catalog.host, "database");
        assert_eq!(catalog.port, 4003);
        assert_eq!(catalog.user, "greptime");
        assert_eq!(catalog.database, "public");
        assert!(Catalog::parse("http://database/public").is_none());
    }

    #[test]
    fn quotes_text_for_sql() {
        assert_eq!(sql_string("a'b"), "'a''b'");
        assert_eq!(sql_number(Some(41.5)), "41.500000");
        assert_eq!(sql_number(None), "NULL");
    }

    #[test]
    fn reads_a_data_row() {
        let mut body = vec![0, 2];
        body.extend_from_slice(&5i32.to_be_bytes());
        body.extend_from_slice(b"a.jpg");
        body.extend_from_slice(&(-1i32).to_be_bytes());
        let fields = parse_row(&body).unwrap();
        assert_eq!(fields[0].as_deref(), Some("a.jpg"));
        assert!(fields[1].is_none());
    }

    #[test]
    fn parses_a_stored_timestamp() {
        let when = parse_timestamp("2014-03-22 18:04:05.000000").unwrap();
        assert_eq!(when.format("%Y-%m-%d %H:%M:%S").to_string(), "2014-03-22 18:04:05");
    }

    #[test]
    fn stores_and_loads_a_picture_when_the_database_is_set() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let catalog = Catalog::parse(&url).expect("database address");
        let folder = format!("/tmp/timeline-catalog-{}", std::process::id());
        let path = format!("{folder}/kept.jpg");
        let picture = Picture {
            id: 1,
            path: PathBuf::from(&path),
            captured_at: parse_timestamp("2014-03-22 18:04:05").unwrap(),
            name: "kept.jpg".to_string(),
            latitude: Some(41.01),
            longitude: Some(28.97),
            place: Some("Istanbul".to_string()),
        };
        let mut session = catalog.open().expect("database connection");
        session.prepare().unwrap();
        session.remove_folder(&folder).unwrap();
        session.save_pictures(&folder, &[&picture]).unwrap();
        let known = session.load_folder(&folder).unwrap();
        session.remove_folder(&folder).unwrap();

        let stored = known.get(&path).expect("stored picture");
        assert_eq!(stored.captured_at, picture.captured_at);
        assert_eq!(stored.latitude, Some(41.01));
        assert_eq!(stored.longitude, Some(28.97));
        assert_eq!(stored.place.as_deref(), Some("Istanbul"));
    }
}
