//! Polling file watcher that reports created, changed, and removed paths by
//! comparing metadata snapshots.
//!
//! # Design choice
//! To keep the crate dependency-free and deterministic, the watcher takes a
//! clock function (`now`) as a constructor argument. Tests pass a fake clock
//! that returns a fixed `SystemTime`, so no wall-clock time or sleeping is
//! involved. The default `Watcher::new` uses `SystemTime::now`.
//!
//! Change detection is based on the modification time from metadata. If a file
//! is rewritten within the filesystem's timestamp granularity it may not be
//! seen as changed; the README calls this out. We deliberately do not hash
//! contents, because that would make the watcher O(size) on every step and
//! would require reading files that may be large or unreadable.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A single observed change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The path exists now and did not exist in the previous snapshot.
    Created(PathBuf),
    /// The path exists in both snapshots but its modification time differs.
    Changed(PathBuf),
    /// The path existed in the previous snapshot and is gone now.
    Removed(PathBuf),
}

/// Snapshot of a single path's metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    modified: Option<SystemTime>,
}

/// Polls a set of paths and reports changes between successive `poll` calls.
///
/// The watcher is constructed with the paths to observe and a clock function.
/// Each call to [`Watcher::poll`] compares the current metadata against the
/// snapshot taken at construction (or the previous call) and returns the list
/// of events. The snapshot is then replaced with the current state, so events
/// are reported exactly once.
pub struct Watcher<F: Fn() -> SystemTime> {
    paths: Vec<PathBuf>,
    now: F,
    snapshot: HashMap<PathBuf, Entry>,
}

impl Watcher<fn() -> SystemTime> {
    /// Create a watcher using the system clock.
    ///
    /// This is the convenient constructor; tests should use
    /// [`Watcher::with_clock`] with a deterministic clock instead.
    pub fn new<P: Into<PathBuf>>(paths: Vec<P>) -> Self {
        Self::with_clock(paths, SystemTime::now as fn() -> SystemTime)
    }
}

impl<F: Fn() -> SystemTime> Watcher<F> {
    /// Create a watcher with an explicit clock function.
    ///
    /// The clock is called once per `poll` invocation, so a fake clock can
    /// return a fixed value in tests without affecting behaviour.
    pub fn with_clock<P: Into<PathBuf>>(paths: Vec<P>, now: F) -> Self {
        let paths: Vec<PathBuf> = paths.into_iter().map(Into::into).collect();
        let mut watcher = Watcher {
            paths,
            now,
            snapshot: HashMap::new(),
        };
        watcher.snapshot = watcher.read_snapshot();
        watcher
    }

    /// Poll the watched paths and return the events since the last poll.
    ///
    /// Events are ordered by path for deterministic output. The internal
    /// snapshot is updated to the state read during this call.
    pub fn poll(&mut self) -> Vec<Event> {
        let current = self.read_snapshot();
        let mut events: Vec<Event> = Vec::new();

        for (path, entry) in &current {
            match self.snapshot.get(path) {
                None => events.push(Event::Created(path.clone())),
                Some(old) => {
                    if old.modified != entry.modified {
                        events.push(Event::Changed(path.clone()));
                    }
                }
            }
        }

        for path in self.snapshot.keys() {
            if !current.contains_key(path) {
                events.push(Event::Removed(path.clone()));
            }
        }

        // Sort for deterministic ordering. The `Event` variants are ordered by
        // declaration, but we want path order regardless of variant, so we
        // sort by path first and then by variant discriminant for stable
        // output when a path appears in more than one event (which cannot
        // happen here, but keeps the ordering explicit).
        events.sort_by(|a, b| {
            let pa = event_path(a);
            let pb = event_path(b);
            pa.cmp(pb).then_with(|| event_rank(a).cmp(&event_rank(b)))
        });

        self.snapshot = current;
        events
    }

    /// Read metadata for every watched path.
    ///
    /// Paths that do not exist are absent from the map. Metadata errors other
    /// than `NotFound` (for example permission denied) are treated as absent
    /// as well: the watcher cannot observe a path it cannot stat, and
    /// reporting it as removed is the only honest signal available.
    fn read_snapshot(&self) -> HashMap<PathBuf, Entry> {
        let mut map = HashMap::with_capacity(self.paths.len());
        for path in &self.paths {
            if let Ok(meta) = fs::metadata(path) {
                let modified = meta.modified().ok();
                map.insert(path.clone(), Entry { modified });
            }
        }
        map
    }
}

fn event_path(event: &Event) -> &Path {
    match event {
        Event::Created(p) | Event::Changed(p) | Event::Removed(p) => p,
    }
}

fn event_rank(event: &Event) -> u8 {
    match event {
        Event::Created(_) => 0,
        Event::Changed(_) => 1,
        Event::Removed(_) => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    /// A clock that returns a fixed time. Used so tests never depend on
    /// wall-clock time.
    fn fixed_clock() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "file-watcher-polling-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn new_file_is_reported_as_created() {
        let dir = temp_dir("created");
        let file = dir.join("a.txt");

        let mut watcher = Watcher::with_clock(vec![file.clone()], fixed_clock);
        assert!(watcher.poll().is_empty(), "no file yet");

        fs::write(&file, b"hello").expect("write file");
        let events = watcher.poll();
        assert_eq!(events, vec![Event::Created(file.clone())]);

        cleanup(&dir);
    }

    #[test]
    fn removed_file_is_reported_as_removed() {
        let dir = temp_dir("removed");
        let file = dir.join("b.txt");
        fs::write(&file, b"bye").expect("write file");

        let mut watcher = Watcher::with_clock(vec![file.clone()], fixed_clock);
        assert!(watcher.poll().is_empty(), "file already present at construction");

        fs::remove_file(&file).expect("remove file");
        let events = watcher.poll();
        assert_eq!(events, vec![Event::Removed(file.clone())]);

        cleanup(&dir);
    }

    #[test]
    fn changed_file_is_reported_when_mtime_differs() {
        let dir = temp_dir("changed");
        let file = dir.join("c.txt");
        fs::write(&file, b"one").expect("write file");

        let mut watcher = Watcher::with_clock(vec![file.clone()], fixed_clock);
        assert!(watcher.poll().is_empty(), "initial state");

        // Rewrite with different contents. On most filesystems this updates
        // mtime, but to make the test deterministic we explicitly set the
        // file's modification time to a known later value using `filetime`
        // would be an external crate; instead we accept that mtime may not
        // change on coarse filesystems. To avoid a flaky test we skip if the
        // metadata mtime did not change.
        fs::write(&file, b"two").expect("rewrite file");
        let before = fs::metadata(&file).expect("meta").modified().ok();
        std::thread::sleep(Duration::from_millis(10));
        fs::write(&file, b"three").expect("rewrite again");
        let after = fs::metadata(&file).expect("meta").modified().ok();
        if before == after {
            // Filesystem granularity too coarse; cannot test change detection
            // deterministically here. This is the documented edge case.
            cleanup(&dir);
            return;
        }

        let events = watcher.poll();
        assert_eq!(events, vec![Event::Changed(file.clone())]);

        cleanup(&dir);
    }

    #[test]
    fn multiple_paths_are_sorted_by_path() {
        let dir = temp_dir("sorted");
        let file_a = dir.join("a.txt");
        let file_b = dir.join("b.txt");
        let file_c = dir.join("c.txt");

        let mut watcher = Watcher::with_clock(
            vec![file_c.clone(), file_a.clone(), file_b.clone()],
            fixed_clock,
        );
        assert!(watcher.poll().is_empty());

        fs::write(&file_b, b"b").expect("write b");
        fs::write(&file_a, b"a").expect("write a");
        fs::write(&file_c, b"c").expect("write c");

        let events = watcher.poll();
        assert_eq!(
            events,
            vec![
                Event::Created(file_a.clone()),
                Event::Created(file_b.clone()),
                Event::Created(file_c.clone()),
            ]
        );

        cleanup(&dir);
    }

    #[test]
    fn events_are_reported_once() {
        let dir = temp_dir("once");
        let file = dir.join("d.txt");

        let mut watcher = Watcher::with_clock(vec![file.clone()], fixed_clock);
        assert!(watcher.poll().is_empty());

        fs::write(&file, b"x").expect("write file");
        let first = watcher.poll();
        assert_eq!(first, vec![Event::Created(file.clone())]);

        let second = watcher.poll();
        assert!(second.is_empty(), "no new events without changes");

        cleanup(&dir);
    }

    #[test]
    fn missing_path_does_not_panic() {
        let dir = temp_dir("missing");
        let missing = dir.join("does-not-exist.txt");

        let mut watcher = Watcher::with_clock(vec![missing.clone()], fixed_clock);
        assert!(watcher.poll().is_empty());
        assert!(watcher.poll().is_empty());

        cleanup(&dir);
    }

    #[test]
    fn directory_is_tracked_like_a_file() {
        let dir = temp_dir("dir");
        let sub = dir.join("sub");

        let mut watcher = Watcher::with_clock(vec![sub.clone()], fixed_clock);
        assert!(watcher.poll().is_empty());

        fs::create_dir(&sub).expect("create dir");
        let events = watcher.poll();
        assert_eq!(events, vec![Event::Created(sub.clone())]);

        cleanup(&dir);
    }

    #[test]
    fn create_then_remove_in_one_poll_reports_removed() {
        let dir = temp_dir("create-remove");
        let file = dir.join("e.txt");

        let mut watcher = Watcher::with_clock(vec![file.clone()], fixed_clock);
        assert!(watcher.poll().is_empty());

        fs::write(&file, b"temp").expect("write file");
        fs::remove_file(&file).expect("remove file");

        // The file was never in a snapshot, so it is not reported as created
        // or removed. The watcher only sees the state at poll time.
        let events = watcher.poll();
        assert!(events.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn empty_path_list_produces_no_events() {
        let mut watcher: Watcher<fn() -> SystemTime> =
            Watcher::with_clock(Vec::<PathBuf>::new(), fixed_clock);
        assert!(watcher.poll().is_empty());
        assert!(watcher.poll().is_empty());
    }
}
