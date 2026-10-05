//! A route's file is either one the build can write, or none.
//!
//! The pre-render (`seo`) writes one HTML file per route. An author's page is
//! written under the author's shown name, and the profile registry puts no
//! bound on a handle's length. A name past what a filesystem takes for one
//! component (255 bytes on the usual ones) gives a file name no `fs::write`
//! accepts, so `Route::file` answers `None` for it, as it does for a name
//! that would leave its directory, and the build skips the route.

use app::route::Route;

#[test]
fn a_name_too_long_for_a_file_gets_no_file() {
    let handle = "a".repeat(300);
    let route = Route::Author {
        handle: handle.clone(),
    };
    let Some(file) = route.file() else {
        // No file: the build skips the route, as it does one that would leave
        // its directory.
        return;
    };

    // The build would write it here: it must be a file that can be written.
    let dir = std::env::temp_dir().join(format!(
        "shonin-route-file-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let target = dir.join(&file);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let written = std::fs::write(&target, "<html></html>");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        written.is_ok(),
        "the route says its page is written to {file:?}, and writing it fails: {}",
        written.unwrap_err()
    );
}

#[test]
fn a_name_as_long_as_a_file_name_may_be_still_gets_its_file() {
    let handle = "a".repeat(app::route::FILE_NAME_BYTES);
    let route = Route::Author { handle };
    assert!(route.file().is_some());
    let route = Route::Author {
        handle: "a".repeat(app::route::FILE_NAME_BYTES + 1),
    };
    assert_eq!(route.file(), None);
}
