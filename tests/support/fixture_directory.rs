//! Distinct directories for test fixtures.
//!
//! A wall-clock reading is not a unique name. `SystemTime` on Windows advances
//! in 100 nanosecond steps, and every test in one process shares
//! `std::process::id`. Two callers can therefore build one path. The sequence
//! below is a process-wide atomic counter, so a value is handed out once.
//!
//! The directory is created with [`std::fs::create_dir`]. `create_dir_all` is
//! not used: it succeeds when the directory is already present, and the callers
//! would then share it. `AlreadyExists` advances the counter and the call tries
//! the next name.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static FIXTURE_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn create_unique_fixture_directory(purpose: &str) -> PathBuf {
    assert_purpose_is_a_single_segment(purpose);
    let process_identifier = std::process::id();
    let name_prefix = format!("ahi-{purpose}-{process_identifier}-");
    allocate_distinct_directory(
        &std::env::temp_dir(),
        &name_prefix,
        &FIXTURE_DIRECTORY_SEQUENCE,
    )
}

fn allocate_distinct_directory(parent: &Path, name_prefix: &str, sequence: &AtomicU64) -> PathBuf {
    assert_prefix_stays_in_parent(name_prefix);
    loop {
        let ordinal = sequence.fetch_add(1, Ordering::Relaxed);
        let directory = parent.join(format!("{name_prefix}{ordinal}"));
        match fs::create_dir(&directory) {
            Ok(()) => return directory,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!(
                "failed to create fixture directory {}: {error}",
                directory.display()
            ),
        }
    }
}

fn assert_purpose_is_a_single_segment(purpose: &str) {
    let letters_digits_or_hyphen = !purpose.is_empty()
        && purpose
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    assert!(
        letters_digits_or_hyphen,
        "fixture purpose must be letters, digits, and hyphens"
    );
}

fn assert_prefix_stays_in_parent(name_prefix: &str) {
    assert!(
        !name_prefix.is_empty()
            && !name_prefix.contains('/')
            && !name_prefix.contains('\\')
            && name_prefix != "."
            && name_prefix != "..",
        "fixture directory prefix must be a single path segment"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use std::thread;

    const CONCURRENT_CALLER_COUNT: usize = 8;
    const DIRECTORIES_PER_CALLER: usize = 64;

    struct DirectoryRemovalGuard {
        directories: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl Drop for DirectoryRemovalGuard {
        fn drop(&mut self) {
            if let Ok(directories) = self.directories.lock() {
                for directory in directories.iter() {
                    let _ = fs::remove_dir_all(directory);
                }
            }
        }
    }

    #[test]
    fn concurrent_callers_receive_distinct_fixture_directories() {
        let recorded = Arc::new(Mutex::new(Vec::with_capacity(
            CONCURRENT_CALLER_COUNT * DIRECTORIES_PER_CALLER,
        )));
        let _removal_guard = DirectoryRemovalGuard {
            directories: Arc::clone(&recorded),
        };
        let mut callers = Vec::with_capacity(CONCURRENT_CALLER_COUNT);
        for _ in 0..CONCURRENT_CALLER_COUNT {
            let recorded = Arc::clone(&recorded);
            callers.push(thread::spawn(move || {
                for _ in 0..DIRECTORIES_PER_CALLER {
                    let directory = create_unique_fixture_directory("contention");
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(directory.join("ownership"))
                        .unwrap_or_else(|error| {
                            panic!(
                                "fixture directory {} is shared or was not created: {error}",
                                directory.display()
                            )
                        });
                    recorded
                        .lock()
                        .expect("fixture directory list")
                        .push(directory);
                }
            }));
        }
        for caller in callers {
            caller.join().expect("fixture directory caller");
        }
        let directories = recorded.lock().expect("fixture directory list");
        assert_eq!(
            directories.len(),
            CONCURRENT_CALLER_COUNT * DIRECTORIES_PER_CALLER
        );
        let mut distinct = HashSet::with_capacity(directories.len());
        for directory in directories.iter() {
            assert!(
                distinct.insert(directory.clone()),
                "fixture directory allocated twice: {}",
                directory.display()
            );
            assert!(directory.is_dir());
        }
    }

    #[test]
    fn occupied_directory_name_advances_to_the_next_ordinal() {
        let parent = create_unique_fixture_directory("occupied-parent");
        let _removal_guard = DirectoryRemovalGuard {
            directories: Arc::new(Mutex::new(vec![parent.clone()])),
        };
        let sequence = AtomicU64::new(0);
        let occupied = parent.join("ahi-occupied-0");
        fs::create_dir(&occupied).unwrap();
        let allocated = allocate_distinct_directory(&parent, "ahi-occupied-", &sequence);
        assert_eq!(allocated, parent.join("ahi-occupied-1"));
        assert!(occupied.is_dir());
        assert!(allocated.is_dir());
        let error = fs::create_dir(&allocated).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(sequence.load(Ordering::Relaxed), 2);
    }
}
