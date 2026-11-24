# file-watcher-polling

Polls a set of paths at intervals and reports changed, created, or removed entries by comparing metadata.

```rust
use file_watcher_polling::{Event, Watcher};
use std::path::PathBuf;

let paths: Vec<PathBuf> = vec!["data.txt".into()];
let mut watcher = Watcher::new(paths);

// First poll establishes the baseline; it returns no events.
let _ = watcher.poll();

// ... later, after the file has been created or modified ...
for event in watcher.poll() {
    match event {
        Event::Created(p) => println!("created: {}", p.display()),
        Event::Changed(p) => println!("changed: {}", p.display()),
        Event::Removed(p) => println!("removed: {}", p.display()),
    }
}
```

## Why this exists

Most file watchers rely on OS notification APIs (inotify, kqueue, ReadDirectoryChangesW) or on third-party crates. Those are fast, but they are also platform-specific and add dependencies. This crate is for the cases where you want a tiny, portable, dependency-free check: you already have a loop, you can afford to call `poll` on your own schedule, and you only need to know whether a path's metadata changed since the last time you looked.

The trade-off is latency and granularity. Nothing happens between calls to `poll`; if a file is created and deleted before the next call, the watcher never sees it. Change detection compares the modification time reported by `std::fs::metadata`, so a file rewritten within the filesystem's timestamp granularity may be reported as unchanged. The library deliberately does not hash contents: doing so would read every watched file on every poll, which is the opposite of a cheap metadata check.

## API

- `Watcher::new(paths)` — build a watcher using the system clock.
- `Watcher::with_clock(paths, now)` — build a watcher with an explicit clock function; useful when you want deterministic behaviour in tests.
- `Watcher::poll(&mut self) -> Vec<Event>` — compare current metadata against the snapshot taken at construction or the previous `poll`, return the events, and update the snapshot.
- `Event::Created(PathBuf)`, `Event::Changed(PathBuf)`, `Event::Removed(PathBuf)` — the three kinds of change.

Events are sorted by path, so the order is deterministic. Each event is reported at most once: after a `poll`, the snapshot is replaced with the current state.

## The awkward edge

A path that is created and removed between two `poll` calls produces no event, because the watcher only sees the state at poll time. Likewise, a file rewritten within the filesystem's timestamp granularity may not be reported as changed. If you need to catch those cases, this library is the wrong tool; use an OS notification API or hash contents yourself.
