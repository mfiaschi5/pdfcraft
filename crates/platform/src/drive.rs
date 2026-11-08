//! Google Drive for desktop's folders on this computer. PdfCraft never talks to Google: it opens
//! and saves in the folders Drive for desktop keeps in sync, and Drive uploads them. No sign-in, no
//! network code, and it works offline.
//!
//! - **macOS:** one folder per account, `~/Library/CloudStorage/GoogleDrive-<account>`, holding
//!   the account's places (My Drive, Shared drives, Other computers, named in the system's
//!   language). macOS asks the user before an app may read them; until it is allowed, listing
//!   fails with [`ListError::NotAllowed`].
//! - **Windows:** Drive for desktop mounts a drive letter (`G:`) holding the places; the account
//!   isn't in the path. Not yet tried on a Windows machine.
//! - **Linux:** there is no Drive for desktop.
//!
//! Listing is bounded ([`MAX_SCANNED`], [`MAX_FOLDERS`], [`MAX_FILES`]) and names typed by the
//! user are checked ([`check_name`]) before anything is made or renamed, so a name can't climb out
//! of the folder chosen. A document opened from these folders is an ordinary file: saving it
//! writes the synced copy, and Drive uploads that.

use std::path::{Path, PathBuf};

/// Entries read from one folder.
pub const MAX_SCANNED: usize = 50_000;
/// Folders returned from one folder.
pub const MAX_FOLDERS: usize = 5_000;
/// PDFs returned from one folder.
pub const MAX_FILES: usize = 5_000;
/// Accounts looked at.
pub const MAX_ACCOUNTS: usize = 32;
/// Longest file or folder name, in bytes (the limit of the file systems Drive syncs to).
pub const MAX_NAME_BYTES: usize = 255;

/// The account's own folder holding its places, in the languages Drive for desktop names it.
const MY_DRIVE: &[&str] = &["My Drive", "Il mio Drive", "Mi unidad", "Mon Drive", "Meine Ablage", "Meu Drive", "Mijn Drive", "Min enhet", "Mój dysk"];

/// One Google account synced by Drive for desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// The account's address on macOS, the drive letter (`G:`) on Windows.
    pub name: String,
    /// The folder holding its places.
    pub root: PathBuf,
}

/// Why a folder couldn't be listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListError {
    /// The system hasn't allowed PdfCraft to read it (macOS asks the user first).
    NotAllowed,
    /// It isn't there any more.
    Missing,
    Other(String),
}

/// What is wrong with a typed file or folder name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    /// A character no synced file system takes in a name (`/ \ : * ? " < > |`, control characters).
    Character(char),
    /// `.`/`..`, a name starting with a dot (hidden), ending in a dot or space, or a Windows
    /// device name (`CON`, `NUL`, `COM1` …).
    Reserved,
    TooLong,
    /// Something of that name is already there.
    Exists,
    Other(String),
}

/// The Drive accounts on this computer, by name.
pub fn accounts(home: Option<&Path>) -> Vec<Account> {
    let mut found = home.map(mac_accounts).unwrap_or_default();
    #[cfg(windows)]
    found.extend(windows_drives());
    found.truncate(MAX_ACCOUNTS);
    found
}

/// `~/Library/CloudStorage/GoogleDrive-<account>` (looked for on every system, so it can be tested).
fn mac_accounts(home: &Path) -> Vec<Account> {
    let Ok(dir) = std::fs::read_dir(home.join("Library/CloudStorage")) else { return Vec::new() };
    let mut found: Vec<Account> = dir
        .filter_map(Result::ok)
        .take(256)
        .filter_map(|e| {
            let name = e.file_name().to_str()?.strip_prefix("GoogleDrive-")?.to_owned();
            (!name.is_empty()).then(|| Account { name, root: e.path() })
        })
        .collect();
    found.sort_by_key(|a| a.name.to_lowercase());
    found
}

/// Drive letters holding a My Drive folder.
#[cfg(windows)]
fn windows_drives() -> Vec<Account> {
    (b'D'..=b'Z')
        .filter_map(|letter| {
            let root = PathBuf::from(format!("{}:\\", letter as char));
            MY_DRIVE.iter().any(|n| root.join(n).is_dir()).then(|| Account { name: format!("{}:", letter as char), root })
        })
        .collect()
}

/// Which of an account's places is My Drive, if one is.
pub fn my_drive(places: &[String]) -> Option<&str> {
    places.iter().find(|p| MY_DRIVE.contains(&p.as_str())).map(String::as_str)
}

/// A folder, or a PDF, in a Drive folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// A folder, or a link to one; else a PDF.
    pub folder: bool,
    /// The PDF's size in bytes (0 for a folder).
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

/// The folders in `dir`, sorted by name, without hidden ones or names that aren't Unicode.
pub fn folders(dir: &Path) -> Result<Vec<String>, ListError> {
    Ok(entries(dir)?.into_iter().filter(|e| e.folder).map(|e| e.name).collect())
}

/// The folders in `dir`, then its PDFs, each sorted by name, without hidden ones or names that
/// aren't Unicode. At most [`MAX_FOLDERS`] folders and [`MAX_FILES`] PDFs. A PDF Drive keeps
/// online only is listed like any other: Drive downloads it when it is read.
pub fn entries(dir: &Path) -> Result<Vec<Entry>, ListError> {
    let list = std::fs::read_dir(dir).map_err(|e| list_error(&e))?;
    let (mut folders, mut files) = (Vec::new(), Vec::new());
    for entry in list.take(MAX_SCANNED) {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        // Only what is listed is looked at further: a folder needs no more, a PDF its size.
        let meta = if kind.is_dir() {
            None
        } else if kind.is_symlink() || (kind.is_file() && is_pdf(&name)) {
            // A shortcut or link counts as what it points to.
            match std::fs::metadata(entry.path()) {
                Ok(m) => Some(m),
                Err(_) => continue,
            }
        } else {
            continue;
        };
        match meta {
            None => folders.push(Entry { name, folder: true, size: 0, modified: None }),
            Some(m) if m.is_dir() => folders.push(Entry { name, folder: true, size: 0, modified: m.modified().ok() }),
            Some(m) if m.is_file() && is_pdf(&name) => files.push(Entry { name, folder: false, size: m.len(), modified: m.modified().ok() }),
            Some(_) => {}
        }
    }
    folders.sort_by_cached_key(|e| e.name.to_lowercase());
    folders.truncate(MAX_FOLDERS);
    files.sort_by_cached_key(|e| e.name.to_lowercase());
    files.truncate(MAX_FILES);
    folders.append(&mut files);
    Ok(folders)
}

/// Whether `name` is a PDF's, by its extension.
pub fn is_pdf(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Where a path is in Google Drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// Which of the accounts given.
    pub account: usize,
    /// The names from the account's folder down to the path, its place (My Drive …) first.
    pub parts: Vec<String>,
}

/// Where `path` is in one of `accounts`, if it is in one: the first whose folder holds it. A
/// path that climbs (`..`) or whose names aren't Unicode isn't placed.
pub fn place(path: &Path, accounts: &[Account]) -> Option<Place> {
    accounts.iter().enumerate().find_map(|(account, a)| {
        let rest = path.strip_prefix(&a.root).ok()?;
        let parts = rest
            .components()
            .map(|c| match c {
                std::path::Component::Normal(n) => n.to_str().map(str::to_owned),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Place { account, parts })
    })
}

/// Rename the file or folder at `path` to `name`, in the same folder, and return its new path. A
/// PDF keeps its `.pdf` extension. Refused when something of that name is there already, except
/// the file itself under another case. That is checked just before renaming, and the system's
/// rename would replace a file made in between.
pub fn rename(path: &Path, name: &str) -> Result<PathBuf, NameError> {
    let (Some(parent), Some(old)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Err(NameError::Other("not a file in a folder".into()));
    };
    let name = if path.is_file() && is_pdf(old) { pdf_name(name)? } else { check_name(name)?.to_owned() };
    if name == old {
        return Ok(path.to_path_buf());
    }
    let dest = parent.join(&name);
    // The exact name taken, or another name the file system takes as the same (case, Unicode
    // normalization) that isn't just this file in another case.
    let exact = std::fs::read_dir(parent)
        .map_err(|e| NameError::Other(e.to_string()))?
        .take(MAX_SCANNED)
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_str() == Some(name.as_str()));
    let same = dest.symlink_metadata().is_ok() && !name.to_lowercase().eq(&old.to_lowercase());
    if exact || same {
        return Err(NameError::Exists);
    }
    std::fs::rename(path, &dest).map_err(|e| NameError::Other(e.to_string()))?;
    Ok(dest)
}

fn list_error(e: &std::io::Error) -> ListError {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => ListError::NotAllowed,
        std::io::ErrorKind::NotFound => ListError::Missing,
        _ => ListError::Other(e.to_string()),
    }
}

/// `name` trimmed, if it can be a file or folder name everywhere Drive syncs to.
pub fn check_name(name: &str) -> Result<&str, NameError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if let Some(c) = name.chars().find(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(NameError::Character(c));
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.ends_with(|c: char| c.is_ascii_digit()));
    if name.starts_with('.') || name.ends_with('.') || device {
        return Err(NameError::Reserved);
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(NameError::TooLong);
    }
    Ok(name)
}

/// The file name for a document called `name`, with the `.pdf` extension it needs.
pub fn pdf_name(name: &str) -> Result<String, NameError> {
    let name = check_name(name)?;
    let full = if name.to_ascii_lowercase().ends_with(".pdf") { name.to_owned() } else { format!("{name}.pdf") };
    check_name(&full).map(str::to_owned)
}

/// Make the folder `name` in `parent`, and return it.
pub fn new_folder(parent: &Path, name: &str) -> Result<PathBuf, NameError> {
    let path = parent.join(check_name(name)?);
    match std::fs::create_dir(&path) {
        Ok(()) => Ok(path),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(NameError::Exists),
        Err(e) => Err(NameError::Other(e.to_string())),
    }
}

#[cfg(test)]
mod tests;
