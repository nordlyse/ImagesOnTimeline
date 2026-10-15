mod files;
mod library;
mod scan;
mod store;

use std::env;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::files::read_picture;
use crate::library::{display_label, list_folders, resolve_folder, FolderView};
use crate::scan::{scan_folder, Picture};
use crate::store::{fresh_pictures, Catalog, FolderScan, Session};

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_CSS: &str = include_str!("../static/app.css");
const APP_JS: &str = include_str!("../static/app.js");

#[derive(Clone)]
struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    library_root: PathBuf,
    host_label: String,
    catalog: Option<Catalog>,
    library: Mutex<Library>,
}

struct Library {
    label: Option<String>,
    scanned_at: Option<DateTime<Utc>>,
    pictures: Vec<Picture>,
}

struct ScannedFolder {
    label: String,
    scanned_at: DateTime<Utc>,
    reused: bool,
    pictures: Vec<Picture>,
}

struct Incoming {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

struct Outgoing {
    status: u16,
    content_type: &'static str,
    cache: Option<&'static str>,
    body: Vec<u8>,
}

fn main() -> ExitCode {
    let library_root = PathBuf::from(env::var("LIBRARY_ROOT").unwrap_or_else(|_| "/host".to_string()));
    let host_label = env::var("HOST_ROOT_LABEL").unwrap_or_default();
    let port = env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let catalog = env::var("DATABASE_URL").ok().and_then(|url| match Catalog::parse(&url) {
        Some(catalog) => {
            eprintln!("picture catalog at {}", catalog.label());
            Some(catalog)
        }
        None => {
            eprintln!("DATABASE_URL is not a postgres address");
            None
        }
    });
    let library = recall_library(catalog.as_ref(), &library_root, &host_label);
    let state = AppState {
        inner: Arc::new(Inner {
            library_root,
            host_label,
            catalog,
            library: Mutex::new(library),
        }),
    };
    let address = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&address) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("unable to listen on {address}: {err}");
            return ExitCode::from(1);
        }
    };
    eprintln!("timeline listening on {address}");
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let state = state.clone();
        thread::spawn(move || {
            if let Err(err) = serve(state, stream) {
                eprintln!("connection stopped: {err}");
            }
        });
    }
    ExitCode::SUCCESS
}

fn serve(state: AppState, mut stream: TcpStream) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|err| err.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .map_err(|err| err.to_string())?;
    let request = read_request(&mut stream)?;
    let response = route(state, request);
    write_response(&mut stream, &response)
}

fn route(state: AppState, request: Incoming) -> Outgoing {
    if request.method == "GET" && request.path == "/" {
        return page(INDEX_HTML, "text/html; charset=utf-8");
    }
    if request.method == "GET" && request.path == "/app.css" {
        return page(APP_CSS, "text/css; charset=utf-8");
    }
    if request.method == "GET" && request.path == "/app.js" {
        return page(APP_JS, "text/javascript; charset=utf-8");
    }
    if request.method == "GET" && request.path == "/api/library" {
        return library_status(&state);
    }
    if request.method == "GET" && request.path == "/api/library/folders" {
        return library_folders(&state, &request.query);
    }
    if request.method == "POST" && request.path == "/api/library" {
        return choose_library(&state, &request.body);
    }
    if request.method == "GET" && request.path == "/api/pictures" {
        return picture_list(&state);
    }
    if request.method == "GET" {
        if let Some(id) = request
            .path
            .strip_prefix("/api/pictures/")
            .and_then(|rest| rest.strip_suffix("/file"))
            .and_then(|id| id.parse::<i64>().ok())
        {
            return picture_file(&state, id);
        }
    }
    text(404, "Not found.")
}

fn library_status(state: &AppState) -> Outgoing {
    let library = match state.inner.library.lock() {
        Ok(library) => library,
        Err(_) => return text(500, "The folder query stopped."),
    };
    let label = match &library.label {
        Some(label) => json_string(label),
        None => "null".to_string(),
    };
    let scanned_at = match library.scanned_at {
        Some(scanned_at) => json_string(&scanned_at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        None => "null".to_string(),
    };
    json(200, format!("{{\"label\":{label},\"scanned_at\":{scanned_at}}}"))
}

fn library_folders(state: &AppState, query: &str) -> Outgoing {
    let requested = query_value(query, "path");
    let view = match list_folders(&state.inner.library_root, &state.inner.host_label, &requested) {
        Ok(view) => view,
        Err(message) => return text(400, &message),
    };
    let scanned_at = resolve_folder(&state.inner.library_root, &state.inner.host_label, &requested)
        .ok()
        .and_then(|folder| stored_scan_time(state.inner.catalog.as_ref(), &folder));
    json(200, folder_json(&view, scanned_at.as_ref()))
}

fn choose_library(state: &AppState, body: &[u8]) -> Outgoing {
    if body.len() > 8192 {
        return text(400, "The folder request is too large.");
    }
    let Some(requested) = json_path(&String::from_utf8_lossy(body)) else {
        return text(400, "Choose a folder.");
    };
    eprintln!("show folder {requested}");
    let scanned = match scan_chosen(
        &state.inner.library_root,
        &state.inner.host_label,
        &requested,
        state.inner.catalog.as_ref(),
    ) {
        Ok(scanned) => scanned,
        Err(message) => return text(400, &message),
    };
    let mut library = match state.inner.library.lock() {
        Ok(library) => library,
        Err(_) => return text(500, "The folder scan stopped."),
    };
    let files = scanned.pictures.len();
    let label = scanned.label;
    let scanned_at = scanned.scanned_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    library.label = Some(label.clone());
    library.scanned_at = Some(scanned.scanned_at);
    library.pictures = scanned.pictures;
    json(
        200,
        format!(
            "{{\"label\":{},\"files\":{files},\"scanned_at\":{},\"reused\":{}}}",
            json_string(&label),
            json_string(&scanned_at),
            if scanned.reused { "true" } else { "false" }
        ),
    )
}

fn picture_list(state: &AppState) -> Outgoing {
    let library = match state.inner.library.lock() {
        Ok(library) => library,
        Err(_) => return text(500, "The picture query stopped."),
    };
    let mut body = String::from("[");
    for (index, picture) in library.pictures.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        let captured_at = picture.captured_at.to_rfc3339_opts(SecondsFormat::Secs, true);
        body.push_str(&format!(
            "{{\"id\":{},\"captured_at\":{},\"name\":{},\"latitude\":{},\"longitude\":{},\"place\":{}}}",
            picture.id,
            json_string(&captured_at),
            json_string(&picture.name),
            json_number(picture.latitude),
            json_number(picture.longitude),
            json_optional(picture.place.as_deref())
        ));
    }
    body.push(']');
    json(200, body)
}

fn picture_file(state: &AppState, id: i64) -> Outgoing {
    let stored = {
        let library = match state.inner.library.lock() {
            Ok(library) => library,
            Err(_) => return text(500, "The picture query stopped."),
        };
        library
            .pictures
            .iter()
            .find(|picture| picture.id == id)
            .map(|picture| picture.path.clone())
    };
    let Some(stored) = stored else {
        return text(404, "That picture was not found.");
    };
    match read_picture(&state.inner.library_root, &stored) {
        Some((bytes, content_type)) => Outgoing {
            status: 200,
            content_type,
            cache: Some("private, max-age=3600"),
            body: bytes,
        },
        None => text(404, "That picture was not found."),
    }
}

fn scan_chosen(
    library_root: &Path,
    host_label: &str,
    requested: &str,
    catalog: Option<&Catalog>,
) -> Result<ScannedFolder, String> {
    let folder = resolve_folder(library_root, host_label, requested)?;
    let root = library_root
        .canonicalize()
        .unwrap_or_else(|_| library_root.to_path_buf());
    let folder_key = folder.to_string_lossy().into_owned();
    let label = display_label(&root, host_label, &folder);
    let mut session = catalog.and_then(open_catalog);
    if let Some(session) = session.as_mut() {
        if let Some(stored) = remembered_folder(session, &folder_key) {
            eprintln!(
                "opened stored folder {folder_key} with {} pictures",
                stored.pictures.len()
            );
            return Ok(ScannedFolder {
                label,
                scanned_at: stored.scan.scanned_at,
                reused: true,
                pictures: stored.pictures,
            });
        }
    }
    let known = match session.as_mut() {
        Some(session) => match session.load_folder(&folder_key) {
            Ok(stored) => stored,
            Err(err) => {
                eprintln!("picture catalog read stopped: {err}");
                std::collections::HashMap::new()
            }
        },
        None => std::collections::HashMap::new(),
    };
    let pictures = scan_folder(&folder, &known);
    let scanned_at = Utc::now();
    if let Some(session) = session.as_mut() {
        let fresh = fresh_pictures(&pictures, &known);
        if fresh.is_empty() {
            eprintln!("picture catalog already holds every name in {folder_key}");
        } else if let Err(err) = session.save_pictures(&folder_key, &fresh) {
            eprintln!("picture catalog save stopped: {err}");
        } else {
            eprintln!("picture catalog stored {} new names in {folder_key}", fresh.len());
        }
        if let Err(err) = session.save_scan(&folder_key, scanned_at, pictures.len() as i64) {
            eprintln!("picture catalog folder save stopped: {err}");
        } else {
            eprintln!("picture catalog stored the folder scan for {folder_key}");
        }
    }
    Ok(ScannedFolder {
        label,
        scanned_at,
        reused: false,
        pictures,
    })
}

fn recall_library(catalog: Option<&Catalog>, library_root: &Path, host_label: &str) -> Library {
    let Some(catalog) = catalog else {
        return empty_library();
    };
    let Some(mut session) = open_catalog(catalog) else {
        return empty_library();
    };
    let stored = match remembered_latest(&mut session) {
        Ok(stored) => stored,
        Err(err) => {
            eprintln!("picture catalog folder read stopped: {err}");
            return empty_library();
        }
    };
    let Some(stored) = stored else {
        eprintln!("picture catalog has no stored folder");
        return empty_library();
    };
    let root = library_root
        .canonicalize()
        .unwrap_or_else(|_| library_root.to_path_buf());
    let label = display_label(&root, host_label, Path::new(&stored.scan.folder));
    eprintln!(
        "opened stored folder {} with {} pictures",
        stored.scan.folder,
        stored.pictures.len()
    );
    Library {
        label: Some(label),
        scanned_at: Some(stored.scan.scanned_at),
        pictures: stored.pictures,
    }
}

struct StoredFolder {
    scan: FolderScan,
    pictures: Vec<Picture>,
}

fn remembered_latest(session: &mut Session) -> Result<Option<StoredFolder>, String> {
    if let Some(scan) = session.load_latest_scan()? {
        let pictures = session.load_pictures(&scan.folder)?;
        return Ok(Some(StoredFolder { scan, pictures }));
    }
    let Some(folder) = session.latest_stored_folder()? else {
        return Ok(None);
    };
    remembered_pictures(session, &folder)
}

fn remembered_folder(session: &mut Session, folder: &str) -> Option<StoredFolder> {
    match session.load_scan(folder) {
        Ok(Some(_)) => match remembered_pictures(session, folder) {
            Ok(stored) => stored,
            Err(err) => {
                eprintln!("picture catalog picture read stopped: {err}");
                None
            }
        },
        Ok(None) => match session.load_folder(folder) {
            Ok(known) if !known.is_empty() => match remembered_pictures(session, folder) {
                Ok(stored) => stored,
                Err(err) => {
                    eprintln!("picture catalog picture read stopped: {err}");
                    None
                }
            },
            Ok(_) => None,
            Err(err) => {
                eprintln!("picture catalog read stopped: {err}");
                None
            }
        },
        Err(err) => {
            eprintln!("picture catalog folder read stopped: {err}");
            None
        }
    }
}

fn remembered_pictures(session: &mut Session, folder: &str) -> Result<Option<StoredFolder>, String> {
    let pictures = session.load_pictures(folder)?;
    let scanned_at = match session.load_scan(folder)? {
        Some(scan) => scan.scanned_at,
        None => {
            let scanned_at = session.latest_stored_time(folder)?.unwrap_or_else(Utc::now);
            session.save_scan(folder, scanned_at, pictures.len() as i64)?;
            scanned_at
        }
    };
    Ok(Some(StoredFolder {
        scan: FolderScan {
            folder: folder.to_string(),
            scanned_at,
            picture_count: pictures.len() as i64,
        },
        pictures,
    }))
}

fn stored_scan_time(catalog: Option<&Catalog>, folder: &Path) -> Option<DateTime<Utc>> {
    let folder_key = folder.to_string_lossy();
    let mut session = open_catalog(catalog?)?;
    match session.load_scan(&folder_key) {
        Ok(scan) => scan.map(|scan| scan.scanned_at),
        Err(err) => {
            eprintln!("picture catalog folder read stopped: {err}");
            None
        }
    }
}

fn open_catalog(catalog: &Catalog) -> Option<Session> {
    match catalog.open() {
        Ok(mut session) => match session.prepare() {
            Ok(()) => Some(session),
            Err(err) => {
                eprintln!("picture catalog prepare stopped: {err}");
                None
            }
        },
        Err(err) => {
            eprintln!("picture catalog connection stopped: {err}");
            None
        }
    }
}

fn empty_library() -> Library {
    Library {
        label: None,
        scanned_at: None,
        pictures: Vec::new(),
    }
}

fn folder_json(view: &FolderView, scanned_at: Option<&DateTime<Utc>>) -> String {
    let parent = match &view.parent {
        Some(parent) => json_string(parent),
        None => "null".to_string(),
    };
    let mut entries = String::from("[");
    for (index, entry) in view.entries.iter().enumerate() {
        if index > 0 {
            entries.push(',');
        }
        entries.push_str(&format!(
            "{{\"name\":{},\"label\":{}}}",
            json_string(&entry.name),
            json_string(&entry.label)
        ));
    }
    entries.push(']');
    let scanned_at = match scanned_at {
        Some(scanned_at) => json_string(&scanned_at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        None => "null".to_string(),
    };
    format!(
        "{{\"label\":{},\"parent\":{parent},\"entries\":{entries},\"scanned_at\":{scanned_at}}}",
        json_string(&view.label)
    )
}

fn read_request(stream: &mut TcpStream) -> Result<Incoming, String> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).map_err(|err| err.to_string())?;
        if read == 0 {
            return Err("the request ended before its headers".to_string());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_header_end(&buffer) {
            break end;
        }
        if buffer.len() > 65_536 {
            return Err("the request headers are too large".to_string());
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.lines();
    let start = lines.next().unwrap_or("");
    let mut parts = start.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = target.split_once('?').map_or((target.clone(), String::new()), |(path, query)| {
        (path.to_string(), query.to_string())
    });
    let length = content_length(&head).min(8192);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).map_err(|err| err.to_string())?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Ok(Incoming {
        method,
        path,
        query,
        body,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(head: &str) -> usize {
    for line in head.lines() {
        let Some(value) = line.split_once(':') else {
            continue;
        };
        if value.0.eq_ignore_ascii_case("content-length") {
            return value.1.trim().parse().unwrap_or(0);
        }
    }
    0
}

fn write_response(stream: &mut TcpStream, response: &Outgoing) -> Result<(), String> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    if let Some(cache) = response.cache {
        head.push_str("Cache-Control: ");
        head.push_str(cache);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).map_err(|err| err.to_string())?;
    stream.write_all(&response.body).map_err(|err| err.to_string())?;
    Ok(())
}

fn query_value(query: &str, key: &str) -> String {
    let prefix = format!("{key}=");
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix(&prefix) {
            return url_decode(value);
        }
    }
    String::new()
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        if bytes[index] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_path(body: &str) -> Option<String> {
    let key = "\"path\"";
    let index = body.find(key)?;
    let rest = body[index + key.len()..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            return Some(out);
        }
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let hex: String = chars.by_ref().take(4).collect();
                let code = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    None
}

fn json_number(value: Option<f64>) -> String {
    match value {
        Some(value) if value.is_finite() => format!("{value:.6}"),
        _ => "null".to_string(),
    }
}

fn json_optional(value: Option<&str>) -> String {
    match value {
        Some(value) => json_string(value),
        None => "null".to_string(),
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other.is_control() => out.push_str(&format!("\\u{:04x}", other as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn page(body: &'static str, content_type: &'static str) -> Outgoing {
    Outgoing {
        status: 200,
        content_type,
        cache: Some("no-cache"),
        body: body.as_bytes().to_vec(),
    }
}

fn json(status: u16, body: String) -> Outgoing {
    Outgoing {
        status,
        content_type: "application/json",
        cache: None,
        body: body.into_bytes(),
    }
}

fn text(status: u16, body: &str) -> Outgoing {
    Outgoing {
        status,
        content_type: "text/plain; charset=utf-8",
        cache: None,
        body: body.as_bytes().to_vec(),
    }
}
