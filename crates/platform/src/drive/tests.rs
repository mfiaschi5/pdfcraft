use super::*;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pdfcraft-drive-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn accounts_and_their_places_are_found_where_drive_for_desktop_keeps_them() {
    let home = dir("home");
    assert!(accounts(Some(&home)).is_empty());
    let cloud = home.join("Library/CloudStorage");
    for (folder, places) in
        [("GoogleDrive-b@example.com", &["Il mio Drive", "Altri computer"][..]), ("GoogleDrive-a@example.com", &["My Drive"]), ("Dropbox", &["x"])]
    {
        for p in places {
            std::fs::create_dir_all(cloud.join(folder).join(p)).unwrap();
        }
    }
    std::fs::create_dir_all(cloud.join("GoogleDrive-")).unwrap();
    let found = accounts(Some(&home));
    let names: Vec<&str> = found.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["a@example.com", "b@example.com"], "only Drive's, sorted, none nameless");
    let places = folders(&found[1].root).unwrap();
    assert_eq!(places, ["Altri computer", "Il mio Drive"]);
    assert_eq!(my_drive(&places), Some("Il mio Drive"));
    assert_eq!(my_drive(&["Other computers".to_owned()]), None);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn folders_are_listed_sorted_without_files_or_hidden_ones() {
    let d = dir("list");
    for f in ["beta", "Alpha", ".hidden", "gamma"] {
        std::fs::create_dir(d.join(f)).unwrap();
    }
    std::fs::write(d.join("file.pdf"), b"%PDF").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(d.join("gamma"), d.join("link to gamma")).unwrap();
    let mut want = vec!["Alpha", "beta", "gamma"];
    if cfg!(unix) {
        want.push("link to gamma");
    }
    assert_eq!(folders(&d).unwrap(), want);
    assert_eq!(folders(&d.join("gone")), Err(ListError::Missing));
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(unix)]
#[test]
fn a_folder_the_system_wont_open_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let d = dir("closed");
    let closed = d.join("closed");
    std::fs::create_dir(&closed).unwrap();
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads anything; the check only means something for an ordinary user.
    if std::fs::read_dir(&closed).is_err() {
        assert_eq!(folders(&closed), Err(ListError::NotAllowed));
    }
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn names_that_could_leave_the_folder_or_break_a_synced_copy_are_refused() {
    assert_eq!(check_name("  Notes 2026 "), Ok("Notes 2026"));
    assert_eq!(check_name(""), Err(NameError::Empty));
    assert_eq!(check_name("   "), Err(NameError::Empty));
    for (bad, c) in [("../up", '/'), ("a\\b", '\\'), ("a:b", ':'), ("a?b", '?'), ("a\nb", '\n'), ("a|b", '|')] {
        assert_eq!(check_name(bad), Err(NameError::Character(c)), "{bad}");
    }
    for bad in [".", "..", ".hidden", "trailing.", "CON", "nul.pdf", "com1", "LPT9.txt"] {
        assert_eq!(check_name(bad), Err(NameError::Reserved), "{bad}");
    }
    assert_eq!(check_name("console"), Ok("console"));
    assert_eq!(check_name("COM10"), Ok("COM10"));
    assert_eq!(check_name(&"é".repeat(128)), Err(NameError::TooLong));
    assert_eq!(pdf_name("Report").as_deref(), Ok("Report.pdf"));
    assert_eq!(pdf_name("Report.PDF").as_deref(), Ok("Report.PDF"));
    assert_eq!(pdf_name(&"a".repeat(252)), Err(NameError::TooLong), "too long once .pdf is added");
}

#[test]
fn a_new_folder_is_made_inside_the_one_chosen() {
    let d = dir("new");
    let made = new_folder(&d, " Papers ").unwrap();
    assert_eq!(made, d.join("Papers"));
    assert!(made.is_dir());
    assert_eq!(new_folder(&d, "Papers"), Err(NameError::Exists));
    assert_eq!(new_folder(&d, "../escape"), Err(NameError::Character('/')));
    assert!(!d.parent().unwrap().join("escape").exists());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_folder_lists_its_folders_then_its_pdfs_with_their_sizes() {
    let d = dir("entries");
    for f in ["Zeta", "alpha", ".hidden"] {
        std::fs::create_dir(d.join(f)).unwrap();
    }
    for (f, bytes) in [("b.pdf", &b"%PDF-1.7 b"[..]), ("A.PDF", b"%PDF"), ("notes.txt", b"x"), (".secret.pdf", b"%PDF")] {
        std::fs::write(d.join(f), bytes).unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(d.join("b.pdf"), d.join("link.pdf")).unwrap();
    let got = entries(&d).unwrap();
    let names: Vec<(&str, bool)> = got.iter().map(|e| (e.name.as_str(), e.folder)).collect();
    let mut want = vec![("alpha", true), ("Zeta", true), ("A.PDF", false), ("b.pdf", false)];
    if cfg!(unix) {
        want.push(("link.pdf", false));
    }
    assert_eq!(names, want, "folders first, then PDFs, by name; no other files or hidden ones");
    let b = got.iter().find(|e| e.name == "b.pdf").unwrap();
    assert_eq!(b.size, 10);
    assert!(b.modified.is_some());
    assert_eq!(entries(&d.join("gone")), Err(ListError::Missing));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_path_is_placed_in_the_account_whose_folder_holds_it() {
    let a = Account { name: "a@example.com".into(), root: PathBuf::from("/h/Library/CloudStorage/GoogleDrive-a@example.com") };
    let b = Account { name: "b@example.com".into(), root: PathBuf::from("/h/Library/CloudStorage/GoogleDrive-b@example.com") };
    let accounts = [a.clone(), b.clone()];
    assert_eq!(
        place(&b.root.join("My Drive/Papers/x.pdf"), &accounts),
        Some(Place { account: 1, parts: vec!["My Drive".into(), "Papers".into(), "x.pdf".into()] })
    );
    assert_eq!(place(&a.root, &accounts), Some(Place { account: 0, parts: Vec::new() }));
    assert_eq!(place(Path::new("/h/Documents/x.pdf"), &accounts), None);
    assert_eq!(place(&a.root.join("My Drive/../../x.pdf"), &accounts), None, "a path that climbs isn't placed");
    assert_eq!(place(Path::new("/h/Library/CloudStorage/GoogleDrive-a@example.com2/x.pdf"), &accounts), None, "by whole names");
}

#[test]
fn a_file_is_renamed_in_its_folder_and_keeps_being_a_pdf() {
    let d = dir("rename");
    let file = d.join("draft.pdf");
    std::fs::write(&file, b"%PDF").unwrap();
    std::fs::write(d.join("taken.pdf"), b"%PDF").unwrap();
    std::fs::create_dir(d.join("Folder")).unwrap();
    let renamed = rename(&file, " Final ").unwrap();
    assert_eq!(renamed, d.join("Final.pdf"));
    assert!(renamed.is_file() && !file.exists());
    assert_eq!(rename(&renamed, "taken"), Err(NameError::Exists));
    assert_eq!(rename(&renamed, "../up"), Err(NameError::Character('/')));
    assert_eq!(rename(&renamed, ".hidden"), Err(NameError::Reserved));
    assert!(renamed.is_file(), "a refused name leaves the file as it was");
    assert_eq!(rename(&renamed, "Final.pdf").unwrap(), renamed, "the same name is no change");
    // Only the case changes: the file itself, never another one.
    let upper = rename(&renamed, "FINAL").unwrap();
    assert_eq!(upper, d.join("FINAL.pdf"));
    assert_eq!(
        std::fs::read_dir(&d).unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case("final.pdf")).count(),
        1
    );
    // A folder takes the name as typed.
    let folder = rename(&d.join("Folder"), "Archive 2026").unwrap();
    assert_eq!(folder, d.join("Archive 2026"));
    assert!(folder.is_dir());
    let _ = std::fs::remove_dir_all(&d);
}
