use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub struct FolderView {
    pub label: String,
    pub parent: Option<String>,
    pub entries: Vec<FolderEntry>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FolderEntry {
    pub name: String,
    pub label: String,
}

pub fn container_path(library_root: &Path, host_label: &str, requested: &str) -> PathBuf {
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return library_root.to_path_buf();
    }
    let label = host_label.trim_end_matches('/');
    if !label.is_empty() && (trimmed == label || trimmed.starts_with(&format!("{label}/"))) {
        let rest = trimmed[label.len()..].trim_start_matches('/');
        return if rest.is_empty() {
            library_root.to_path_buf()
        } else {
            library_root.join(rest)
        };
    }
    PathBuf::from(trimmed)
}

pub fn display_label(library_root: &Path, host_label: &str, path: &Path) -> String {
    let label = host_label.trim_end_matches('/');
    let Ok(rest) = path.strip_prefix(library_root) else {
        return path.display().to_string();
    };
    if rest.as_os_str().is_empty() {
        return if label.is_empty() {
            library_root.display().to_string()
        } else {
            label.to_string()
        };
    }
    if label.is_empty() {
        library_root.join(rest).display().to_string()
    } else {
        format!("{label}/{}", rest.display())
    }
}

pub fn resolve_folder(library_root: &Path, host_label: &str, requested: &str) -> Result<PathBuf, String> {
    let root = library_root
        .canonicalize()
        .map_err(|_| "The library root is not available.".to_string())?;
    let candidate = container_path(&root, host_label, requested);
    let folder = candidate
        .canonicalize()
        .map_err(|_| "That folder was not found.".to_string())?;
    if !folder.starts_with(&root) {
        return Err("That folder is outside the library root.".to_string());
    }
    if !folder.is_dir() {
        return Err("Choose a folder.".to_string());
    }
    Ok(folder)
}

pub fn list_folders(library_root: &Path, host_label: &str, requested: &str) -> Result<FolderView, String> {
    let folder = resolve_folder(library_root, host_label, requested)?;
    let root = library_root
        .canonicalize()
        .map_err(|_| "The library root is not available.".to_string())?;
    let mut entries = Vec::new();
    let children = fs::read_dir(&folder).map_err(|err| format!("unable to read folder: {err}"))?;
    for child in children {
        let child = match child {
            Ok(child) => child,
            Err(_) => continue,
        };
        let name = child.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let path = child.path();
        if !is_directory(&path) {
            continue;
        }
        entries.push(FolderEntry {
            name: name.to_string(),
            label: display_label(&root, host_label, &folder.join(name)),
        });
    }
    entries.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    let parent = if folder == root {
        None
    } else {
        folder.parent().map(|parent| display_label(&root, host_label, parent))
    };
    Ok(FolderView {
        label: display_label(&root, host_label, &folder),
        parent,
        entries,
    })
}

fn is_directory(path: &Path) -> bool {
    let path = path.to_path_buf();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let is_dir = fs::symlink_metadata(&path)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        let _ = sender.send(is_dir);
    });
    receiver.recv_timeout(Duration::from_millis(400)).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("timeline-library-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("photos/trip")).unwrap();
        fs::write(root.join("photos/note.txt"), b"x").unwrap();
        fs::create_dir(root.join(".hidden")).unwrap();
        root
    }

    #[test]
    fn maps_a_host_label_into_the_library_root() {
        let root = Path::new("/host");
        assert_eq!(
            container_path(root, "/Users/xman", "/Users/xman/Pictures"),
            PathBuf::from("/host/Pictures")
        );
        assert_eq!(
            container_path(root, "/Users/xman", "/Users/xman"),
            PathBuf::from("/host")
        );
    }

    #[test]
    fn lists_child_folders_and_rejects_paths_outside_the_root() {
        let root = sample_root();
        let view = list_folders(&root, "/Users/xman", "").unwrap();
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].name, "photos");
        assert!(view.parent.is_none());

        let nested = list_folders(&root, "/Users/xman", &view.entries[0].label).unwrap();
        assert_eq!(nested.entries[0].name, "trip");
        assert!(nested.parent.is_some());

        let outside = list_folders(&root, "/Users/xman", "/etc");
        assert!(outside.is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
