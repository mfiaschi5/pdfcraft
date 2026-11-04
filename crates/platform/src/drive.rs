//! Google Drive for desktop's folders on this computer. PdfCraft never talks to Google: it saves
//! into the folders Drive for desktop keeps in sync, and Drive uploads them. No sign-in, no
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
//! Listing is bounded ([`MAX_SCANNED`], [`MAX_FOLDERS`]) and names typed by the user are checked
//! ([`check_name`]) before anything is created, so a name can't climb out of the folder chosen.

use std::path::{Path, PathBuf};

/// Entries read from one folder.
pub const MAX_SCANNED: usize = 50_000;
/// Folders returned from one folder.
pub const MAX_FOLDERS: usize = 5_000;
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

/// The folders in `dir`, sorted by name, without hidden ones or names that aren't Unicode.
pub fn folders(dir: &Path) -> Result<Vec<String>, ListError> {
    let entries = std::fs::read_dir(dir).map_err(|e| list_error(&e))?;
    let mut out: Vec<String> = Vec::new();
    for entry in entries.take(MAX_SCANNED) {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if name.starts_with('.') {
            continue;
        }
        // A shortcut or link counts when what it points to is a folder.
        let is_dir = match entry.file_type() {
            Ok(t) if t.is_symlink() => entry.path().is_dir(),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            out.push(name);
        }
    }
    out.sort_by_cached_key(|n| n.to_lowercase());
    out.truncate(MAX_FOLDERS);
    Ok(out)
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
