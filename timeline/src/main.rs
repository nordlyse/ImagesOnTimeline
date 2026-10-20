mod files;
mod library;
mod scan;
mod store;

use std::env;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveTime, SecondsFormat, TimeZone, Utc};

use crate::files::{open_inside, read_picture};
use crate::library::{display_label, list_folders, resolve_folder, FolderView};
use crate::scan::{is_video, omit_removed, scan_folder, Picture};
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
    folder: Option<String>,
    label: Option<String>,
    scanned_at: Option<DateTime<Utc>>,
    pictures: Vec<Picture>,
}

struct ScannedFolder {
    folder: String,
    label: String,
    scanned_at: DateTime<Utc>,
    reused: bool,
    pictures: Vec<Picture>,
}

struct Incoming {
    method: String,
    path: String,
    query: String,
    range: Option<ByteRange>,
    body: Vec<u8>,
}

struct ByteRange {
    start: u64,
    end: Option<u64>,
}

struct FileSpan {
    file: std::fs::File,
    start: u64,
    length: u64,
}

struct Outgoing {
    status: u16,
    content_type: &'static str,
    cache: Option<&'static str>,
    file_name: Option<String>,
    accept_ranges: bool,
    content_range: Option<String>,
    span: Option<FileSpan>,
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
        .set_write_timeout(Some(Duration::from_secs(300)))
        .map_err(|err| err.to_string())?;
    let request = read_request(&mut stream)?;
    let mut response = route(state, request);
    write_response(&mut stream, &mut response)
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
    if request.method == "GET" && request.path == "/api/pictures/names" {
        return picture_names(&state, &request.query);
    }
    if request.method == "GET" && request.path == "/api/pictures/file" {
        return picture_download(&state, &request.query, request.range);
    }
    if request.method == "DELETE" && request.path == "/api/pictures/file" {
        return picture_remove_named(&state, &request.query);
    }
    if request.method == "DELETE" {
        if let Some(id) = request
            .path
            .strip_prefix("/api/pictures/")
            .and_then(|id| id.parse::<i64>().ok())
        {
            return picture_remove(&state, id);
        }
    }
    if request.method == "GET" {
        if let Some(id) = request
            .path
            .strip_prefix("/api/pictures/")
            .and_then(|rest| rest.strip_suffix("/file"))
            .and_then(|id| id.parse::<i64>().ok())
        {
            return picture_file(&state, id, request.range);
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
    let payload = String::from_utf8_lossy(body);
    let Some(requested) = json_path(&payload) else {
        return text(400, "Choose a folder.");
    };
    let rescan = json_flag(&payload, "rescan");
    if rescan {
        eprintln!("scan folder {requested}");
    } else {
        eprintln!("show folder {requested}");
    }
    let scanned = match scan_chosen(
        &state.inner.library_root,
        &state.inner.host_label,
        &requested,
        state.inner.catalog.as_ref(),
        rescan,
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
    library.folder = Some(scanned.folder);
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
            "{{\"id\":{},\"captured_at\":{},\"name\":{},\"latitude\":{},\"longitude\":{},\"place\":{},\"video\":{}}}",
            picture.id,
            json_string(&captured_at),
            json_string(&picture.name),
            json_number(picture.latitude),
            json_number(picture.longitude),
            json_optional(picture.place.as_deref()),
            if is_video(&picture.path) { "true" } else { "false" }
        ));
    }
    body.push(']');
    json(200, body)
}

fn picture_names(state: &AppState, query: &str) -> Outgoing {
    let from = match range_bound(query, "from", false) {
        Ok(bound) => bound,
        Err(message) => return text(400, &message),
    };
    let to = match range_bound(query, "to", true) {
        Ok(bound) => bound,
        Err(message) => return text(400, &message),
    };
    if let (Some(from), Some(to)) = (from, to) {
        if from > to {
            return text(400, "The start date is after the end date.");
        }
    }
    let library = match state.inner.library.lock() {
        Ok(library) => library,
        Err(_) => return text(500, "The picture query stopped."),
    };
    let mut body = String::from("[");
    let mut first = true;
    for picture in library.pictures.iter() {
        if !in_range(picture.captured_at, from, to) {
            continue;
        }
        if !first {
            body.push(',');
        }
        first = false;
        body.push_str(&json_string(&picture.name));
    }
    body.push(']');
    json(200, body)
}

fn picture_download(state: &AppState, query: &str, range: Option<ByteRange>) -> Outgoing {
    let name = query_value(query, "name");
    let name = name.trim();
    if name.is_empty() {
        return text(400, "Choose a picture name.");
    }
    let stored = {
        let library = match state.inner.library.lock() {
            Ok(library) => library,
            Err(_) => return text(500, "The picture query stopped."),
        };
        let mut matches = library
            .pictures
            .iter()
            .filter(|picture| picture.name == name)
            .map(|picture| picture.path.clone());
        let Some(path) = matches.next() else {
            return text(404, "That picture was not found.");
        };
        if matches.next().is_some() {
            return text(400, "More than one picture has that name.");
        }
        path
    };
    send_library_file(&state.inner.library_root, &stored, range, Some(name.to_string()))
}

fn picture_remove_named(state: &AppState, query: &str) -> Outgoing {
    let name = query_value(query, "name");
    let name = name.trim();
    if name.is_empty() {
        return text(400, "Choose a picture name.");
    }
    let id = {
        let library = match state.inner.library.lock() {
            Ok(library) => library,
            Err(_) => return text(500, "The picture query stopped."),
        };
        let mut matches = library.pictures.iter().filter(|picture| picture.name == name);
        let Some(picture) = matches.next() else {
            return text(404, "That picture was not found.");
        };
        if matches.next().is_some() {
            return text(400, "More than one picture has that name.");
        }
        picture.id
    };
    picture_remove(state, id)
}

fn picture_remove(state: &AppState, id: i64) -> Outgoing {
    let mut library = match state.inner.library.lock() {
        Ok(library) => library,
        Err(_) => return text(500, "The picture query stopped."),
    };
    let Some(index) = library.pictures.iter().position(|picture| picture.id == id) else {
        return text(404, "That picture was not found.");
    };
    let name = library.pictures[index].name.clone();
    let path = match library.pictures[index].path.to_str() {
        Some(path) => path.to_string(),
        None => return text(500, "The picture could not be removed."),
    };
    if let Some(catalog) = state.inner.catalog.as_ref() {
        let Some(folder) = library.folder.clone() else {
            return text(500, "The picture could not be removed.");
        };
        let Some(mut session) = open_catalog(catalog) else {
            return text(500, "The picture could not be removed.");
        };
        if let Err(err) = session.remove_picture(&folder, &path) {
            eprintln!("picture catalog remove stopped: {err}");
            return text(500, "The picture could not be removed.");
        }
        let count = library.pictures.len().saturating_sub(1) as i64;
        let scanned_at = library.scanned_at.unwrap_or_else(Utc::now);
        if let Err(err) = session.save_picture_count(&folder, scanned_at, count) {
            eprintln!("picture catalog count stopped: {err}");
        }
    }
    library.pictures.remove(index);
    json(200, format!("{{\"id\":{id},\"name\":{}}}", json_string(&name)))
}

fn in_range(captured_at: DateTime<Utc>, from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> bool {
    if let Some(from) = from {
        if captured_at < from {
            return false;
        }
    }
    if let Some(to) = to {
        if captured_at > to {
            return false;
        }
    }
    true
}

fn range_bound(query: &str, key: &str, end_of_day: bool) -> Result<Option<DateTime<Utc>>, String> {
    let value = query_value(query, key);
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    parse_bound(value, end_of_day).map(Some)
}

fn parse_bound(value: &str, end_of_day: bool) -> Result<DateTime<Utc>, String> {
    if let Ok(when) = DateTime::parse_from_rfc3339(value) {
        return Ok(when.with_timezone(&Utc));
    }
    let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") else {
        return Err("Use a date such as 2012-12-06.".to_string());
    };
    let time = if end_of_day {
        NaiveTime::from_hms_nano_opt(23, 59, 59, 999_999_999).unwrap()
    } else {
        NaiveTime::from_hms_opt(0, 0, 0).unwrap()
    };
    Ok(Utc.from_utc_datetime(&date.and_time(time)))
}

fn download_name(name: &str, content_type: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if content_type == "image/jpeg" && !lower.ends_with(".jpg") && !lower.ends_with(".jpeg") {
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        return format!("{stem}.jpg");
    }
    name.to_string()
}

fn picture_file(state: &AppState, id: i64, range: Option<ByteRange>) -> Outgoing {
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
    send_library_file(&state.inner.library_root, &stored, range, None)
}

fn send_library_file(root: &Path, stored: &Path, range: Option<ByteRange>, name: Option<String>) -> Outgoing {
    if is_video(stored) {
        return send_video(root, stored, range, name);
    }
    match read_picture(root, stored) {
        Some((bytes, content_type)) => Outgoing {
            status: 200,
            content_type,
            cache: Some("private, max-age=3600"),
            file_name: name.map(|name| download_name(&name, content_type)),
            accept_ranges: false,
            content_range: None,
            span: None,
            body: bytes,
        },
        None => text(404, "That picture was not found."),
    }
}

fn send_video(root: &Path, stored: &Path, range: Option<ByteRange>, file_name: Option<String>) -> Outgoing {
    let Some((file, total, content_type)) = open_inside(root, stored) else {
        return text(404, "That picture was not found.");
    };
    let sliced = match slice_range(total, range) {
        Ok(bounds) => bounds,
        Err(()) => {
            return Outgoing {
                status: 416,
                content_type: "text/plain; charset=utf-8",
                cache: None,
                file_name: None,
                accept_ranges: true,
                content_range: Some(format!("bytes */{total}")),
                span: None,
                body: b"That range is outside the video.".to_vec(),
            };
        }
    };
    let (status, start, length, content_range) = match sliced {
        None => (200, 0, total, None),
        Some((start, end)) => {
            let length = end - start + 1;
            (206, start, length, Some(format!("bytes {start}-{end}/{total}")))
        }
    };
    Outgoing {
        status,
        content_type,
        cache: Some("private, max-age=3600"),
        file_name,
        accept_ranges: true,
        content_range,
        span: Some(FileSpan { file, start, length }),
        body: Vec::new(),
    }
}

fn slice_range(total: u64, range: Option<ByteRange>) -> Result<Option<(u64, u64)>, ()> {
    let Some(range) = range else {
        return Ok(None);
    };
    if total == 0 || range.start >= total {
        return Err(());
    }
    let end = range.end.unwrap_or(total - 1).min(total - 1);
    if end < range.start {
        return Err(());
    }
    Ok(Some((range.start, end)))
}

fn scan_chosen(
    library_root: &Path,
    host_label: &str,
    requested: &str,
    catalog: Option<&Catalog>,
    rescan: bool,
) -> Result<ScannedFolder, String> {
    let folder = resolve_folder(library_root, host_label, requested)?;
    let root = library_root
        .canonicalize()
        .unwrap_or_else(|_| library_root.to_path_buf());
    let folder_key = folder.to_string_lossy().into_owned();
    let label = display_label(&root, host_label, &folder);
    let mut session = catalog.and_then(open_catalog);
    if !rescan {
        if let Some(session) = session.as_mut() {
            if let Some(mut stored) = remembered_folder(session, &folder_key) {
                match session.load_removed(&folder_key) {
                    Ok(removed) => omit_removed(&mut stored.pictures, &removed),
                    Err(err) => eprintln!("picture catalog removed read stopped: {err}"),
                }
                eprintln!(
                    "opened stored folder {folder_key} with {} pictures",
                    stored.pictures.len()
                );
                return Ok(ScannedFolder {
                    folder: folder_key,
                    label,
                    scanned_at: stored.scan.scanned_at,
                    reused: true,
                    pictures: stored.pictures,
                });
            }
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
    let mut pictures = scan_folder(&folder, &known);
    if let Some(session) = session.as_mut() {
        match session.load_removed(&folder_key) {
            Ok(removed) => omit_removed(&mut pictures, &removed),
            Err(err) => eprintln!("picture catalog removed read stopped: {err}"),
        }
    }
    let scanned_at = Utc::now();
    if let Some(session) = session.as_mut() {
        let fresh = fresh_pictures(&pictures, &known);
        let stored = if rescan {
            if let Err(err) = session.clear_pictures(&folder_key) {
                eprintln!("picture catalog clear stopped: {err}");
                false
            } else if let Err(err) = session.save_pictures(&folder_key, &pictures.iter().collect::<Vec<_>>()) {
                eprintln!("picture catalog save stopped: {err}");
                false
            } else {
                eprintln!("picture catalog stored the new scan for {folder_key}");
                true
            }
        } else if fresh.is_empty() {
            eprintln!("picture catalog already holds every name in {folder_key}");
            true
        } else if let Err(err) = session.save_pictures(&folder_key, &fresh) {
            eprintln!("picture catalog save stopped: {err}");
            false
        } else {
            eprintln!("picture catalog stored {} new names in {folder_key}", fresh.len());
            true
        };
        if !stored {
            eprintln!("picture catalog kept the previous folder scan for {folder_key}");
        } else if let Err(err) = session.save_scan(&folder_key, scanned_at, pictures.len() as i64) {
            eprintln!("picture catalog folder save stopped: {err}");
        } else {
            eprintln!("picture catalog stored the folder scan for {folder_key}");
        }
    }
    Ok(ScannedFolder {
        folder: folder_key,
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
    let Some(mut stored) = stored else {
        eprintln!("picture catalog has no stored folder");
        return empty_library();
    };
    let folder = stored.scan.folder.clone();
    match session.load_removed(&folder) {
        Ok(removed) => omit_removed(&mut stored.pictures, &removed),
        Err(err) => eprintln!("picture catalog removed read stopped: {err}"),
    }
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
        folder: Some(folder),
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
        folder: None,
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
        range: byte_range(&head),
        body,
    })
}

fn byte_range(head: &str) -> Option<ByteRange> {
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.eq_ignore_ascii_case("range") {
            continue;
        }
        let rest = value.trim().strip_prefix("bytes=")?;
        if rest.contains(',') {
            return None;
        }
        let (start, end) = rest.split_once('-')?;
        let start = start.parse().ok()?;
        let end = if end.is_empty() { None } else { Some(end.parse().ok()?) };
        return Some(ByteRange { start, end });
    }
    None
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

fn write_response(stream: &mut TcpStream, response: &mut Outgoing) -> Result<(), String> {
    let reason = match response.status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        _ => "Internal Server Error",
    };
    let length = response.span.as_ref().map(|span| span.length).unwrap_or(response.body.len() as u64);
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {length}\r\nConnection: close\r\n",
        response.status, response.content_type
    );
    if let Some(cache) = response.cache {
        head.push_str("Cache-Control: ");
        head.push_str(cache);
        head.push_str("\r\n");
    }
    if response.accept_ranges {
        head.push_str("Accept-Ranges: bytes\r\n");
    }
    if let Some(range) = &response.content_range {
        head.push_str("Content-Range: ");
        head.push_str(range);
        head.push_str("\r\n");
    }
    if let Some(name) = &response.file_name {
        head.push_str("Content-Disposition: ");
        head.push_str(&content_disposition(name));
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).map_err(|err| err.to_string())?;
    if let Some(span) = response.span.as_mut() {
        span.file.seek(SeekFrom::Start(span.start)).map_err(|err| err.to_string())?;
        let mut left = span.length;
        let mut buf = [0u8; 65536];
        while left > 0 {
            let chunk = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
            let read = span.file.read(&mut buf[..chunk]).map_err(|err| err.to_string())?;
            if read == 0 {
                break;
            }
            stream.write_all(&buf[..read]).map_err(|err| err.to_string())?;
            left -= read as u64;
        }
    } else {
        stream.write_all(&response.body).map_err(|err| err.to_string())?;
    }
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

fn json_flag(body: &str, key: &str) -> bool {
    let needle = format!("\"{key}\"");
    let Some(index) = body.find(&needle) else {
        return false;
    };
    let rest = body[index + needle.len()..].trim_start();
    let Some(rest) = rest.strip_prefix(':') else {
        return false;
    };
    rest.trim_start().starts_with("true")
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

fn content_disposition(name: &str) -> String {
    let mut quoted = String::new();
    let mut encoded = String::new();
    for ch in name.chars() {
        if ch == '"' || ch == '\\' || ch.is_control() {
            quoted.push('_');
        } else {
            quoted.push(ch);
        }
        let mut buf = [0; 4];
        for byte in ch.encode_utf8(&mut buf).bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') {
                encoded.push(byte as char);
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    format!("attachment; filename=\"{quoted}\"; filename*=UTF-8''{encoded}")
}

fn page(body: &'static str, content_type: &'static str) -> Outgoing {
    Outgoing {
        status: 200,
        content_type,
        cache: Some("no-cache"),
        file_name: None,
        accept_ranges: false,
        content_range: None,
        span: None,
        body: body.as_bytes().to_vec(),
    }
}

fn json(status: u16, body: String) -> Outgoing {
    Outgoing {
        status,
        content_type: "application/json",
        cache: None,
        file_name: None,
        accept_ranges: false,
        content_range: None,
        span: None,
        body: body.into_bytes(),
    }
}

fn text(status: u16, body: &str) -> Outgoing {
    Outgoing {
        status,
        content_type: "text/plain; charset=utf-8",
        cache: None,
        file_name: None,
        accept_ranges: false,
        content_range: None,
        span: None,
        body: body.as_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, at: &str) -> Picture {
        Picture {
            id: 1,
            path: PathBuf::from(name),
            captured_at: DateTime::parse_from_rfc3339(at).unwrap().with_timezone(&Utc),
            name: name.to_string(),
            latitude: None,
            longitude: None,
            place: None,
        }
    }

    fn state_with(root: PathBuf, pictures: Vec<Picture>) -> AppState {
        AppState {
            inner: Arc::new(Inner {
                library_root: root,
                host_label: String::new(),
                catalog: None,
                library: Mutex::new(Library {
                    folder: None,
                    label: None,
                    scanned_at: None,
                    pictures,
                }),
            }),
        }
    }

    fn album() -> AppState {
        state_with(
            PathBuf::from("/tmp"),
            vec![
                sample("early.jpg", "2006-07-22T00:05:07Z"),
                sample("kept.jpg", "2012-12-06T15:46:01Z"),
                sample("later.jpg", "2014-03-22T18:04:05Z"),
            ],
        )
    }

    fn get(state: &AppState, path: &str, query: &str) -> Outgoing {
        call(state, "GET", path, query)
    }

    fn delete(state: &AppState, path: &str, query: &str) -> Outgoing {
        call(state, "DELETE", path, query)
    }

    fn call(state: &AppState, method: &str, path: &str, query: &str) -> Outgoing {
        route(
            state.clone(),
            Incoming {
                method: method.to_string(),
                path: path.to_string(),
                query: query.to_string(),
                range: None,
                body: Vec::new(),
            },
        )
    }

    fn body(response: Outgoing) -> String {
        String::from_utf8(response.body).unwrap()
    }

    #[test]
    fn get_lists_every_picture_name() {
        let response = get(&album(), "/api/pictures/names", "");
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "application/json");
        assert_eq!(body(response), "[\"early.jpg\",\"kept.jpg\",\"later.jpg\"]");
    }

    #[test]
    fn get_lists_names_inside_a_date_range() {
        let response = get(&album(), "/api/pictures/names", "from=2012-01-01&to=2012-12-06");
        assert_eq!(response.status, 200);
        assert_eq!(body(response), "[\"kept.jpg\"]");
    }

    #[test]
    fn get_lists_names_from_an_instant() {
        let response = get(
            &album(),
            "/api/pictures/names",
            "from=2012-12-06T15:46:01Z&to=2012-12-06T15:46:01Z",
        );
        assert_eq!(response.status, 200);
        assert_eq!(body(response), "[\"kept.jpg\"]");
    }

    #[test]
    fn get_lists_names_with_one_bound() {
        let from = get(&album(), "/api/pictures/names", "from=2013-01-01");
        assert_eq!(body(from), "[\"later.jpg\"]");
        let to = get(&album(), "/api/pictures/names", "to=2006-12-31");
        assert_eq!(body(to), "[\"early.jpg\"]");
    }

    #[test]
    fn get_rejects_a_reversed_or_invalid_date_range() {
        let reversed = get(&album(), "/api/pictures/names", "from=2014-01-01&to=2012-01-01");
        assert_eq!(reversed.status, 400);
        let invalid = get(&album(), "/api/pictures/names", "from=later");
        assert_eq!(invalid.status, 400);
        assert_eq!(body(invalid), "Use a date such as 2012-12-06.");
    }

    #[test]
    fn get_ignores_a_name_list_that_is_not_get() {
        let response = route(
            album(),
            Incoming {
                method: "POST".to_string(),
                path: "/api/pictures/names".to_string(),
                query: String::new(),
                range: None,
                body: Vec::new(),
            },
        );
        assert_eq!(response.status, 404);
    }

    #[test]
    fn get_downloads_one_named_picture() {
        let root = std::env::temp_dir().join(format!("timeline-download-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("my photo.jpg");
        std::fs::write(&file, b"jpeg-bytes").unwrap();
        let mut picture = sample("my photo.jpg", "2012-12-06T15:46:01Z");
        picture.path = file;
        let state = state_with(root.clone(), vec![picture, sample("other.jpg", "2014-03-22T18:04:05Z")]);
        let response = get(&state, "/api/pictures/file", "name=my%20photo.jpg");
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "image/jpeg");
        assert_eq!(response.body, b"jpeg-bytes");
        assert_eq!(response.file_name.as_deref(), Some("my photo.jpg"));
        let mut copied = response_copy(&state);
        let head = response_head(&mut copied);
        assert!(head.contains("Content-Disposition: attachment; filename=\"my photo.jpg\""));
        let missing = get(&state, "/api/pictures/file", "name=missing.jpg");
        assert_eq!(missing.status, 404);
        let unnamed = get(&state, "/api/pictures/file", "");
        assert_eq!(unnamed.status, 400);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn response_copy(state: &AppState) -> Outgoing {
        get(state, "/api/pictures/file", "name=my%20photo.jpg")
    }

    fn response_head(response: &mut Outgoing) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut stream, &mut buf).unwrap();
            buf
        });
        let (mut server, _) = listener.accept().unwrap();
        write_response(&mut server, response).unwrap();
        drop(server);
        String::from_utf8(client.join().unwrap()).unwrap()
    }

    #[test]
    fn get_refuses_a_name_shared_by_two_pictures() {
        let state = state_with(
            PathBuf::from("/tmp"),
            vec![
                sample("same.jpg", "2012-12-06T15:46:01Z"),
                sample("same.jpg", "2014-03-22T18:04:05Z"),
            ],
        );
        let response = get(&state, "/api/pictures/file", "name=same.jpg");
        assert_eq!(response.status, 400);
        assert_eq!(body(response), "More than one picture has that name.");
    }

    #[test]
    fn get_serves_a_video_in_a_byte_range() {
        let root = std::env::temp_dir().join(format!("timeline-video-range-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("clip.mp4");
        std::fs::write(&file, b"0123456789").unwrap();
        let mut picture = sample("clip.mp4", "2012-12-06T15:46:01Z");
        picture.path = file;
        let state = state_with(root.clone(), vec![picture]);
        let mut response = route(
            state,
            Incoming {
                method: "GET".to_string(),
                path: "/api/pictures/1/file".to_string(),
                query: String::new(),
                range: Some(ByteRange { start: 2, end: Some(5) }),
                body: Vec::new(),
            },
        );
        assert_eq!(response.status, 206);
        assert_eq!(response.content_type, "video/mp4");
        assert!(response.accept_ranges);
        assert_eq!(response.content_range.as_deref(), Some("bytes 2-5/10"));
        let message = response_head(&mut response);
        assert!(message.contains("HTTP/1.1 206 Partial Content"));
        assert!(message.contains("Accept-Ranges: bytes"));
        assert!(message.ends_with("2345"));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn video_library(name: &str, bytes: &[u8]) -> (AppState, PathBuf) {
        let root = std::env::temp_dir().join(format!("timeline-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join(name);
        std::fs::write(&file, bytes).unwrap();
        let mut still = sample("still.png", "2006-07-22T00:05:07Z");
        still.id = 1;
        let mut clip = sample(name, "2012-12-06T15:46:01Z");
        clip.id = 2;
        clip.path = file;
        let state = state_with(root.clone(), vec![still, clip]);
        (state, root)
    }

    #[test]
    fn get_marks_a_video_in_the_picture_list() {
        let response = get(
            &state_with(
                PathBuf::from("/tmp"),
                vec![
                    sample("still.png", "2006-07-22T00:05:07Z"),
                    sample("clip.mp4", "2012-12-06T15:46:01Z"),
                ],
            ),
            "/api/pictures",
            "",
        );
        assert_eq!(response.status, 200);
        let text = body(response);
        assert!(text.contains("\"name\":\"clip.mp4\""));
        assert!(text.contains("\"video\":true"));
        assert!(text.contains("\"name\":\"still.png\""));
        assert!(text.contains("\"video\":false"));
    }

    #[test]
    fn get_lists_a_video_name_inside_its_date() {
        let state = state_with(
            PathBuf::from("/tmp"),
            vec![
                sample("still.png", "2006-07-22T00:05:07Z"),
                sample("clip.mp4", "2012-12-06T15:46:01Z"),
            ],
        );
        let names = get(&state, "/api/pictures/names", "");
        assert_eq!(body(names), "[\"still.png\",\"clip.mp4\"]");
        let ranged = get(&state, "/api/pictures/names", "from=2012-12-06&to=2012-12-06");
        assert_eq!(body(ranged), "[\"clip.mp4\"]");
    }

    #[test]
    fn get_serves_a_whole_video() {
        let (state, root) = video_library("whole.mp4", b"0123456789");
        let mut response = get(&state, "/api/pictures/2/file", "");
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "video/mp4");
        assert!(response.accept_ranges);
        assert!(response.content_range.is_none());
        let message = response_head(&mut response);
        assert!(message.contains("Accept-Ranges: bytes"));
        assert!(message.ends_with("0123456789"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn get_serves_the_tail_of_a_video() {
        let (state, root) = video_library("tail.mp4", b"0123456789");
        let mut response = route(
            state,
            Incoming {
                method: "GET".to_string(),
                path: "/api/pictures/2/file".to_string(),
                query: String::new(),
                range: Some(ByteRange { start: 6, end: None }),
                body: Vec::new(),
            },
        );
        assert_eq!(response.status, 206);
        assert_eq!(response.content_range.as_deref(), Some("bytes 6-9/10"));
        let message = response_head(&mut response);
        assert!(message.ends_with("6789"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn get_refuses_a_video_range_past_the_end() {
        let (state, root) = video_library("past.mp4", b"0123456789");
        let response = route(
            state,
            Incoming {
                method: "GET".to_string(),
                path: "/api/pictures/2/file".to_string(),
                query: String::new(),
                range: Some(ByteRange { start: 50, end: None }),
                body: Vec::new(),
            },
        );
        assert_eq!(response.status, 416);
        assert_eq!(response.content_range.as_deref(), Some("bytes */10"));
        assert!(response.accept_ranges);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn get_downloads_one_named_video() {
        let (state, root) = video_library("clip.mp4", b"0123456789");
        let mut response = get(&state, "/api/pictures/file", "name=clip.mp4");
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "video/mp4");
        assert_eq!(response.file_name.as_deref(), Some("clip.mp4"));
        let message = response_head(&mut response);
        assert!(message.contains("Content-Disposition: attachment; filename=\"clip.mp4\""));
        assert!(message.ends_with("0123456789"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn get_does_not_slice_a_picture() {
        let root = std::env::temp_dir().join(format!("timeline-still-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("still.png");
        std::fs::write(&file, b"still-bytes").unwrap();
        let mut picture = sample("still.png", "2006-07-22T00:05:07Z");
        picture.path = file;
        let state = state_with(root.clone(), vec![picture]);
        let response = route(
            state,
            Incoming {
                method: "GET".to_string(),
                path: "/api/pictures/1/file".to_string(),
                query: String::new(),
                range: Some(ByteRange { start: 0, end: Some(2) }),
                body: Vec::new(),
            },
        );
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "image/png");
        assert!(!response.accept_ranges);
        assert!(response.content_range.is_none());
        assert_eq!(response.body, b"still-bytes");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delete_removes_a_picture_and_leaves_the_file() {
        let root = std::env::temp_dir().join(format!("timeline-remove-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("kept.jpg");
        std::fs::write(&file, b"jpeg-bytes").unwrap();
        let mut kept = sample("kept.jpg", "2012-12-06T15:46:01Z");
        kept.id = 2;
        kept.path = file.clone();
        let mut early = sample("early.jpg", "2006-07-22T00:05:07Z");
        early.id = 1;
        let state = state_with(root.clone(), vec![early, kept]);
        let response = delete(&state, "/api/pictures/2", "");
        assert_eq!(response.status, 200);
        assert_eq!(body(response), "{\"id\":2,\"name\":\"kept.jpg\"}");
        assert_eq!(body(get(&state, "/api/pictures/names", "")), "[\"early.jpg\"]");
        assert!(file.is_file());
        assert_eq!(std::fs::read(&file).unwrap(), b"jpeg-bytes");
        let missing = delete(&state, "/api/pictures/2", "");
        assert_eq!(missing.status, 404);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delete_removes_a_named_video_and_leaves_the_file() {
        let (state, root) = video_library("drop.mp4", b"0123456789");
        let file_route = delete(&state, "/api/pictures/2/file", "");
        assert_eq!(file_route.status, 404);
        let response = delete(&state, "/api/pictures/file", "name=drop.mp4");
        assert_eq!(response.status, 200);
        assert_eq!(body(response), "{\"id\":2,\"name\":\"drop.mp4\"}");
        assert_eq!(body(get(&state, "/api/pictures/names", "")), "[\"still.png\"]");
        let listed = body(get(&state, "/api/pictures", ""));
        assert!(listed.contains("\"name\":\"still.png\""));
        assert!(!listed.contains("drop.mp4"));
        assert_eq!(std::fs::read(root.join("drop.mp4")).unwrap(), b"0123456789");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delete_refuses_a_missing_or_shared_name() {
        let state = state_with(
            PathBuf::from("/tmp"),
            vec![
                sample("same.jpg", "2012-12-06T15:46:01Z"),
                sample("same.jpg", "2014-03-22T18:04:05Z"),
            ],
        );
        let shared = delete(&state, "/api/pictures/file", "name=same.jpg");
        assert_eq!(shared.status, 400);
        assert_eq!(body(shared), "More than one picture has that name.");
        assert_eq!(body(get(&state, "/api/pictures/names", "")), "[\"same.jpg\",\"same.jpg\"]");
        let missing = delete(&state, "/api/pictures/file", "name=missing.jpg");
        assert_eq!(missing.status, 404);
        let unnamed = delete(&state, "/api/pictures/file", "");
        assert_eq!(unnamed.status, 400);
    }
}
