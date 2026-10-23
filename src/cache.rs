//! A local copy of each fleet's document, for reads that must not touch the
//! network or that the caller allows to be stale.
//!
//! The copy lives at `$XDG_CACHE_HOME/s3-fleet/<fleet>.yaml`. Nothing in the
//! library reads it on its own: the CLI stores every document it loads, and
//! reads the copy only for `--allow-stale`, `known-hosts --cached` (which ssh
//! calls up to three times per connection) and shell completion. A
//! [`Cached`] result always carries its age.
//!
//! [`Cache::store`] rewrites the file only when the content changed, since the
//! cache directory may sit on a slow network home. When the content is
//! unchanged it still bumps the mtime, because the age reported is "last
//! confirmed against S3", not "last time the fleet changed"; otherwise an
//! untouched fleet makes a current copy look months old. A changed file is
//! written to a sibling temp file and renamed into place, so a concurrent
//! reader (ssh's KnownHostsCommand) never sees half a document.
//!
//! The functions are synchronous: two small local files do not justify
//! threading async through every caller.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use etcetera::BaseStrategy;

use crate::doc::{DocError, FleetDoc};

/// A cached document and how long ago S3 last confirmed it.
#[derive(Clone, Debug)]
pub struct Cached {
    /// The document.
    pub doc: FleetDoc,
    /// Time since the copy was last stored; zero if the clock went back.
    pub age: Duration,
}

/// A cache read or write that failed.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// No cache directory could be determined (no HOME).
    #[error("find the XDG cache directory")]
    Dir(#[source] etcetera::HomeDirError),
    /// A file operation failed.
    #[error("{op} {}", path.display())]
    Io {
        /// What was being done.
        op: &'static str,
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// The cached copy does not parse.
    #[error("cached {} is invalid", path.display())]
    Document {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        #[source]
        source: DocError,
    },
}

impl CacheError {
    /// Whether the failure is a cache that was never written.
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }
}

/// The cache directory and its per-fleet files.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// A cache in `dir`, created on first store.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The cache in `$XDG_CACHE_HOME/s3-fleet`.
    pub fn xdg() -> Result<Self, CacheError> {
        let base = etcetera::choose_base_strategy().map_err(CacheError::Dir)?;
        Ok(Self::new(
            base.cache_dir()
                .join("s3-fleet"),
        ))
    }

    /// Where a fleet's copy lives.
    pub fn path(&self, fleet: &str) -> PathBuf {
        self.dir
            .join(format!("{fleet}.yaml"))
    }

    /// Store a fleet's document. See the notes for why an unchanged copy is
    /// touched rather than rewritten.
    pub fn store(&self, fleet: &str, doc: &FleetDoc) -> Result<(), CacheError> {
        let path = self.path(fleet);
        let bytes = doc
            .to_yaml()
            .map_err(|source| CacheError::Document {
                path: path.clone(),
                source,
            })?
            .into_bytes();
        match fs::read(&path) {
            Ok(existing) if existing == bytes => return touch(&path),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("read", &path, source)),
        }
        fs::create_dir_all(&self.dir).map_err(|source| io_error("create", &self.dir, source))?;
        let tmp = self
            .dir
            .join(format!("{fleet}.yaml.tmp.{}", std::process::id()));
        fs::write(&tmp, &bytes).map_err(|source| io_error("write", &tmp, source))?;
        fs::rename(&tmp, &path).map_err(|source| io_error("rename into", &path, source))
    }

    /// Read a fleet's copy and its age.
    pub fn load(&self, fleet: &str) -> Result<Cached, CacheError> {
        let path = self.path(fleet);
        let bytes = fs::read(&path).map_err(|source| io_error("read", &path, source))?;
        let modified = fs::metadata(&path)
            .and_then(|m| m.modified())
            .map_err(|source| io_error("stat", &path, source))?;
        let doc = FleetDoc::parse(&bytes).map_err(|source| CacheError::Document {
            path: path.clone(),
            source,
        })?;
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO);
        Ok(Cached { doc, age })
    }
}

fn touch(path: &Path) -> Result<(), CacheError> {
    let times = fs::FileTimes::new().set_modified(SystemTime::now());
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_times(times))
        .map_err(|source| io_error("touch", path, source))
}

fn io_error(op: &'static str, path: &Path, source: io::Error) -> CacheError {
    CacheError::Io {
        op,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkeys::KEY_A;

    fn scratch_dir() -> tempfile::TempDir {
        landlock_test_confine::to_scratch_only(&landlock_test_confine::target_dir());
        let root = landlock_test_confine::scratch_dir("test-tmp");
        fs::create_dir_all(&root).unwrap();
        tempfile::tempdir_in(root).unwrap()
    }

    fn doc(attr: &str) -> FleetDoc {
        let mut doc = FleetDoc::new();
        doc.pin_host("h", 22, vec![KEY_A.to_string()], false)
            .unwrap();
        doc.set_attribute("a", attr)
            .unwrap();
        doc
    }

    fn set_mtime(path: &Path, ago: Duration) {
        let times = fs::FileTimes::new().set_modified(SystemTime::now() - ago);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(times)
            .unwrap();
    }

    #[test]
    fn missing_cache_is_reported_as_missing() {
        let dir = scratch_dir();
        let cache = Cache::new(
            dir.path()
                .join("sub"),
        );
        let err = cache
            .load("f")
            .unwrap_err();
        assert!(err.is_missing(), "{err}");
    }

    #[test]
    fn store_then_load_round_trips() {
        let dir = scratch_dir();
        let cache = Cache::new(
            dir.path()
                .join("sub"),
        );
        cache
            .store("f", &doc("1"))
            .unwrap();
        let cached = cache
            .load("f")
            .unwrap();
        assert_eq!(cached.doc, doc("1"));
        assert!(cached.age < Duration::from_secs(5));
    }

    #[test]
    fn unchanged_store_refreshes_age_without_rewriting() {
        let dir = scratch_dir();
        let cache = Cache::new(
            dir.path()
                .to_path_buf(),
        );
        cache
            .store("f", &doc("1"))
            .unwrap();
        let path = cache.path("f");
        let inode = |p: &Path| {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(p)
                .unwrap()
                .ino()
        };
        let before = inode(&path);
        set_mtime(&path, Duration::from_secs(3600));
        assert!(
            cache
                .load("f")
                .unwrap()
                .age
                >= Duration::from_secs(3599)
        );

        cache
            .store("f", &doc("1"))
            .unwrap();
        assert_eq!(inode(&path), before, "an unchanged copy is not rewritten");
        assert!(
            cache
                .load("f")
                .unwrap()
                .age
                < Duration::from_secs(5),
            "but its age is reset"
        );

        cache
            .store("f", &doc("2"))
            .unwrap();
        assert_ne!(inode(&path), before, "a changed copy is replaced by rename");
        assert_eq!(
            cache
                .load("f")
                .unwrap()
                .doc,
            doc("2")
        );
    }
}
