//! Integration tests for mmce-store.

use mmce_store::Store;

#[test]
fn memory_store_migrates_on_open() {
    let store = Store::open_memory().unwrap();
    // Two successive opens should be no-ops (idempotent migration).
    drop(store);
    let _again = Store::open_memory().unwrap();
}

#[test]
fn file_store_migrates_and_persists() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let store = Store::open(&path).unwrap();
        let id = store.upsert_book("/some/book.cbz", "zip").unwrap();
        store.touch_book(id, 5, 100).unwrap();
    }
    let store = Store::open(&path).unwrap();
    let row = store.get_book_by_path("/some/book.cbz").unwrap().unwrap();
    assert_eq!(row.last_page, 5);
    assert_eq!(row.page_count, Some(100));
    assert_eq!(row.kind, "zip");
}

#[test]
fn bookmark_upsert_is_idempotent() {
    let store = Store::open_memory().unwrap();
    let book = store.upsert_book("/x", "folder").unwrap();
    let a = store.add_bookmark(book, 3, Some("intro")).unwrap();
    let b = store.add_bookmark(book, 3, Some("first look")).unwrap();
    assert_eq!(a, b, "second add at same page must update not duplicate");
    let list = store.list_bookmarks(book).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].label.as_deref(), Some("first look"));
    assert!(store.remove_bookmark(book, 3).unwrap());
    assert!(store.list_bookmarks(book).unwrap().is_empty());
}

#[test]
fn recent_books_sorts_newest_first_and_respects_limit() {
    let store = Store::open_memory().unwrap();
    let a = store.upsert_book("/a", "folder").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = store.upsert_book("/b", "folder").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let _c = store.upsert_book("/c", "folder").unwrap();

    // Touch `a` to bump its recency.
    store.touch_book(a, 0, 10).unwrap();

    let recent = store.recent_books(2).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].id, a, "most-recently-touched wins");
    assert_eq!(recent[1].path, "/c");
    let _ = b;
}

#[test]
fn setting_round_trip() {
    let store = Store::open_memory().unwrap();
    assert_eq!(store.get_setting("theme").unwrap(), None);
    store.set_setting("theme", "dark").unwrap();
    assert_eq!(store.get_setting("theme").unwrap().as_deref(), Some("dark"));
    store.set_setting("theme", "light").unwrap();
    assert_eq!(
        store.get_setting("theme").unwrap().as_deref(),
        Some("light")
    );
}

#[test]
fn prune_history_keeps_newest_n() {
    let store = Store::open_memory().unwrap();
    let ids: Vec<_> = (0..5)
        .map(|i| {
            // 1.1s apart so ordering is deterministic (SQLite stores seconds).
            std::thread::sleep(std::time::Duration::from_millis(1100));
            store.upsert_book(&format!("/b{i}"), "folder").unwrap()
        })
        .collect();

    let removed = store.prune_history(3).unwrap();
    assert_eq!(removed, 2);

    let remaining = store.recent_books(10).unwrap();
    assert_eq!(remaining.len(), 3);
    let remaining_ids: Vec<_> = remaining.iter().map(|b| b.id).collect();
    // Newest three.
    assert_eq!(remaining_ids, vec![ids[4], ids[3], ids[2]]);
}

#[test]
fn profile_round_trip() {
    let store = Store::open_memory().unwrap();
    store.save_profile("default", "a = 1\n").unwrap();
    store.save_profile("night", "bg = 'black'\n").unwrap();
    let profiles = store.list_profiles().unwrap();
    assert_eq!(profiles, vec!["default".to_string(), "night".to_string()]);
    assert_eq!(
        store.get_profile("default").unwrap().as_deref(),
        Some("a = 1\n")
    );
}
