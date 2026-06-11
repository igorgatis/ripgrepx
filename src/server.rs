//! The per-project daemon: holds the index resident in RAM, keeps it fresh, and answers queries
//! over a Unix socket. Binding the socket *is* the single-instance lock — a second daemon that
//! loses the race exits. The daemon serves immediately: a warm start loads the snapshot and answers
//! at once; a cold start answers via a full ripgrep scan (the correct fallback) until the first
//! build finishes. See `docs/indexing.md` and `docs/index-and-storage.md`.

use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use anyhow::Result;
use notify_debouncer_full::new_debouncer;
use notify_debouncer_full::notify::RecursiveMode;

use crate::confirm::SearchOptions;
use crate::index::{self, Index};
use crate::paths;
use crate::proto::{self, Request};

/// How often a `watch` subscriber repaints when nothing changed (keeps the build-progress count and
/// the snapshot age fresh, and detects client disconnect).
const WATCH_HEARTBEAT: Duration = Duration::from_millis(250);

struct Shared {
    index: RwLock<Index>,
    ready: AtomicBool,
    root: PathBuf,
    snapshot: PathBuf,
    /// Cold-build progress (files processed / total to process); only meaningful while building.
    indexed: AtomicUsize,
    total: AtomicUsize,
    /// A change counter + condvar so `watch` wakes immediately on any transition.
    seq: Mutex<u64>,
    seq_cv: Condvar,
}

impl Shared {
    /// Signal a state change (build done, reconcile applied) to wake watchers.
    fn bump(&self) {
        *self.seq.lock().unwrap() += 1;
        self.seq_cv.notify_all();
    }

    /// Block until the change counter moves past `last` or the heartbeat elapses; return the latest.
    fn wait_change(&self, last: u64) -> u64 {
        let g = self.seq.lock().unwrap();
        let (g, _) = self
            .seq_cv
            .wait_timeout_while(g, WATCH_HEARTBEAT, |s| *s == last)
            .unwrap();
        *g
    }
}

/// Run the daemon for `root` in the foreground. Returns once the socket can't be owned (another
/// daemon is already running) or on a fatal error.
pub fn run(root: PathBuf) -> Result<()> {
    let dir = paths::state_dir(&root);
    std::fs::create_dir_all(&dir)?;
    let sock = paths::socket_path(&root);
    let listener = match bind(&sock)? {
        Some(l) => l,
        None => return Ok(()), // another daemon owns this root
    };

    let shared = Arc::new(Shared {
        index: RwLock::new(Index::default()),
        ready: AtomicBool::new(false),
        root: root.clone(),
        snapshot: paths::snapshot_path(&root),
        indexed: AtomicUsize::new(0),
        total: AtomicUsize::new(0),
        seq: Mutex::new(0),
        seq_cv: Condvar::new(),
    });

    spawn_indexer(shared.clone());
    spawn_watcher(shared.clone());

    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let shared = shared.clone();
        let sock = sock.clone();
        std::thread::spawn(move || {
            let _ = handle(conn, &shared, &sock);
        });
    }
    Ok(())
}

/// Warm-start from the snapshot if present (serve immediately), then build/reconcile in the
/// background so the resident index reflects the real tree.
fn spawn_indexer(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        if let Ok(idx) = Index::load(&shared.snapshot) {
            *shared.index.write().unwrap() = idx;
            shared.ready.store(true, Ordering::SeqCst);
            shared.bump();
            // catch changes made while the daemon was down
            let mut idx = shared.index.write().unwrap();
            idx.reconcile(&shared.root);
            let _ = idx.save(&shared.snapshot);
            drop(idx);
            shared.bump();
        } else {
            // Cold build: publish total, then index reporting per-file progress for watchers.
            let paths = index::walk_files(&shared.root);
            shared.total.store(paths.len(), Ordering::Relaxed);
            shared.bump();
            let built = Index::from_paths(&paths, &shared.indexed);
            let _ = built.save(&shared.snapshot);
            *shared.index.write().unwrap() = built;
            shared.ready.store(true, Ordering::SeqCst);
            shared.bump();
        }
    });
}

/// Watch the tree; on a debounced change burst, reconcile the resident index and persist. The
/// reconcile re-walks ignore-aware, so freshly-created ignored files never leak into results.
fn spawn_watcher(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = match new_debouncer(Duration::from_millis(300), None, move |res| {
            let _ = tx.send(res);
        }) {
            Ok(d) => d,
            Err(_) => return,
        };
        if debouncer
            .watch(&shared.root, RecursiveMode::Recursive)
            .is_err()
        {
            return;
        }
        for res in rx {
            if res.is_err() || !shared.ready.load(Ordering::SeqCst) {
                continue;
            }
            let mut idx = shared.index.write().unwrap();
            if idx.reconcile(&shared.root) > 0 {
                let _ = idx.save(&shared.snapshot);
                drop(idx);
                shared.bump();
            }
        }
    });
}

fn handle(mut conn: UnixStream, shared: &Shared, sock: &Path) -> Result<()> {
    let req = proto::read_request(&mut conn)?;
    match req {
        Request::Search { opts, pattern } => {
            content_search(shared, &pattern, opts, &mut conn)?;
        }
        Request::Find { needle, limit } => {
            proto::write_data(&mut conn, &find(shared, &needle, limit as usize))?;
        }
        Request::Status => {
            proto::write_data(&mut conn, &status(shared))?;
        }
        Request::Watch => return watch(shared, &mut conn),
        Request::Shutdown => {
            proto::write_data(&mut conn, b"ok\n")?;
            proto::end_stream(&mut conn)?;
            let _ = std::fs::remove_file(sock);
            std::process::exit(0);
        }
    }
    proto::end_stream(&mut conn)?;
    Ok(())
}

/// Stream a fresh status frame on every change (and on a heartbeat), until the client disconnects.
/// The blocking wait holds no index lock, and rendering only touches the (cheap-while-building)
/// resident index, so an attached watcher does not slow indexing.
fn watch(shared: &Shared, conn: &mut UnixStream) -> Result<()> {
    let mut last = 0;
    loop {
        if proto::write_data(conn, &status(shared)).is_err() {
            return Ok(()); // client went away
        }
        last = shared.wait_change(last);
    }
}

/// Stream content-search results straight to the socket so huge result sets aren't buffered.
fn content_search(
    shared: &Shared,
    pattern: &str,
    opts: SearchOptions,
    conn: &mut UnixStream,
) -> Result<()> {
    if shared.ready.load(Ordering::SeqCst) {
        let idx = shared.index.read().unwrap();
        crate::stream_search(&idx, pattern, opts, |chunk| {
            proto::write_data(&mut *conn, chunk)
        })
    } else {
        // Cold start only: pipelined full scan until the first build finishes. The sink is shared
        // across walk threads, so guard the socket with a mutex.
        let conn = std::sync::Mutex::new(conn);
        crate::stream_full_scan(&shared.root, pattern, opts, |chunk| {
            if let Ok(mut c) = conn.lock() {
                let _ = proto::write_data(&mut **c, chunk);
            }
        })
    }
}

fn find(shared: &Shared, needle: &str, limit: usize) -> Vec<u8> {
    let mut out = String::new();
    if shared.ready.load(Ordering::SeqCst) {
        let idx = shared.index.read().unwrap();
        for p in idx.find(needle, limit) {
            out.push_str(&p.to_string_lossy());
            out.push('\n');
        }
    } else {
        for p in index::walk_files(&shared.root)
            .iter()
            .filter(|p| p.to_string_lossy().contains(needle))
            .take(limit)
        {
            out.push_str(&p.to_string_lossy());
            out.push('\n');
        }
    }
    out.into_bytes()
}

fn status(shared: &Shared) -> Vec<u8> {
    let idx = shared.index.read().unwrap();
    let state = if shared.ready.load(Ordering::SeqCst) {
        "ready".to_string()
    } else {
        let done = shared.indexed.load(Ordering::Relaxed) as u64;
        let total = shared.total.load(Ordering::Relaxed) as u64;
        if total > 0 {
            format!(
                "building {} / {} files",
                crate::status::human_count(done),
                crate::status::human_count(total)
            )
        } else {
            "building (scanning tree...)".to_string()
        }
    };
    crate::status::Status {
        root: &shared.root,
        snapshot: &shared.snapshot,
        running: true,
        state: Some(state),
        files: Some(idx.file_count()),
        trigrams: Some(idx.trigram_count()),
        memory_bytes: Some(idx.memory_bytes()),
    }
    .render()
    .into_bytes()
}

/// Bind the socket, taking ownership of this root. `Ok(None)` means a live daemon already owns it;
/// a stale socket file (no listener) is removed and rebound.
fn bind(sock: &Path) -> Result<Option<UnixListener>> {
    match UnixListener::bind(sock) {
        Ok(l) => Ok(Some(l)),
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            if UnixStream::connect(sock).is_ok() {
                Ok(None)
            } else {
                std::fs::remove_file(sock).ok();
                Ok(Some(UnixListener::bind(sock)?))
            }
        }
        Err(e) => Err(e.into()),
    }
}
