//! `BackupEngine` impl that materializes a versioned mirror of the
//! source on disk.
//!
//! Layout produced by a job `family-photos` on a repo
//! `\\nas\photos-versions` with source `D:\Pictures`:
//!
//! ```text
//! \\nas\photos-versions\
//!   └── family-photos\               ← job_name
//!       ├── Pictures\                ← source basename, mirrors the source tree
//!       │   ├── 2024\
//!       │   │   └── famille.jpg      ← current version, browsable in Explorer
//!       │   └── …
//!       └── .versions\               ← previous versions of overwritten / deleted files
//!           └── Pictures\2024\
//!               ├── famille-2026-04-12-153000.jpg
//!               └── famille-2026-05-01-083044.jpg
//! ```
//!
//! Change detection compares `mtime + size` between source and dest.
//! False positives (mtime touched but content unchanged) cost one
//! extra archived version; never a data loss.
//!
//! `.versions/` is reserved: the engine refuses to back up a source
//! that contains a `.versions` directory at the root level to avoid
//! self-collision.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::backup::{BackupEngine, BackupSource, RetentionOutcome, SnapshotInfo, JOB_TAG_PREFIX};
use crate::config::{Repository as RepoConfig, Retention};

const VERSIONS_DIR: &str = ".versions";

/// Versioned mirror backend.
pub struct MirrorEngine {
    repo: RepoConfig,
}

impl MirrorEngine {
    pub fn new(repo: RepoConfig) -> Self {
        Self { repo }
    }

    fn job_root(&self, job_name: &str) -> PathBuf {
        self.repo.path.join(job_name)
    }

    fn versions_root(&self, job_name: &str) -> PathBuf {
        self.job_root(job_name).join(VERSIONS_DIR)
    }
}

impl BackupEngine for MirrorEngine {
    fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.repo.path).with_context(|| {
            format!(
                "creating mirror destination root `{}`",
                self.repo.path.display()
            )
        })?;
        info!(repository = %self.repo.path.display(), "mirror repository ready");
        Ok(())
    }

    fn backup(&self, job_name: &str, source: BackupSource) -> Result<SnapshotInfo> {
        let job_root = self.job_root(job_name);
        let versions_root = self.versions_root(job_name);
        std::fs::create_dir_all(&job_root)
            .with_context(|| format!("creating job root `{}`", job_root.display()))?;
        std::fs::create_dir_all(&versions_root).with_context(|| {
            format!("creating versions root `{}`", versions_root.display())
        })?;

        let exclude_set = build_exclude_set(&source.excludes)?;
        let timestamp = version_timestamp();

        // Drop missing source paths up front (same as the rustic engine
        // does — template-resolved jobs can list dirs that don't exist
        // on this machine).
        let existing: Vec<PathBuf> = source
            .paths
            .iter()
            .filter(|p| {
                let ok = p.exists();
                if !ok {
                    warn!(path = %p.display(), "source path does not exist, skipping");
                }
                ok
            })
            .cloned()
            .collect();
        if existing.is_empty() {
            anyhow::bail!("no source paths exist on this system — nothing to back up");
        }

        let mut stats = MirrorStats::default();

        for src_root in &existing {
            let src_basename = src_root
                .file_name()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "cannot derive a basename from source path `{}`",
                        src_root.display()
                    )
                })?
                .to_owned();

            // Refuse a source that has a top-level `.versions` of its
            // own — would self-collide with the versions tree.
            if src_root.join(VERSIONS_DIR).is_dir() {
                anyhow::bail!(
                    "source `{}` contains a `.versions` directory at its root, which is \
                     reserved for the mirror engine's archive — rename it before backing up",
                    src_root.display()
                );
            }

            let dest_root = job_root.join(&src_basename);
            let versions_subroot = versions_root.join(&src_basename);
            std::fs::create_dir_all(&dest_root)?;
            std::fs::create_dir_all(&versions_subroot)?;

            let mut seen: HashSet<PathBuf> = HashSet::new();

            sync_source_into_dest(
                src_root,
                &dest_root,
                &versions_subroot,
                &exclude_set,
                &timestamp,
                &mut seen,
                &mut stats,
            )?;

            evict_files_missing_from_source(
                &dest_root,
                &versions_subroot,
                &seen,
                &timestamp,
                &mut stats,
            )?;
        }

        info!(
            job = job_name,
            new = stats.new_files,
            updated = stats.updated_files,
            deleted = stats.deleted_files,
            unchanged = stats.unchanged_files,
            bytes_copied = stats.bytes_copied,
            "mirror backup complete"
        );

        Ok(SnapshotInfo {
            id: format!("mirror-{timestamp}"),
            time: jiff::Timestamp::now().to_string(),
            paths: existing
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            hostname: hostname(),
            tags: vec![format!("{JOB_TAG_PREFIX}{job_name}")],
            total_bytes_processed: Some(stats.bytes_total),
            data_added: Some(stats.bytes_copied),
        })
    }

    fn list_snapshots(&self, _job_name: &str) -> Result<Vec<SnapshotInfo>> {
        // Mirror doesn't have discrete snapshots — its state is the
        // current mirror plus `.versions/`. The dashboard's snapshot
        // sync sees an empty list for mirror jobs; the JobRun model
        // carries the run history instead.
        Ok(Vec::new())
    }

    fn apply_retention(
        &self,
        job_name: &str,
        retention: &Retention,
    ) -> Result<RetentionOutcome> {
        let keep = match retention.keep_versions {
            Some(n) if n > 0 => n as usize,
            _ => return Ok(RetentionOutcome::default()),
        };
        let versions_root = self.versions_root(job_name);
        if !versions_root.exists() {
            return Ok(RetentionOutcome::default());
        }
        prune_versions(&versions_root, keep)
    }
}

// ---------------------------------------------------------------------
// Walking + sync
// ---------------------------------------------------------------------

#[derive(Debug, Default)]
struct MirrorStats {
    new_files: usize,
    updated_files: usize,
    deleted_files: usize,
    unchanged_files: usize,
    bytes_copied: u64,
    bytes_total: u64,
}

/// Walk `src_root` and bring `dest_root` in line with it. For each
/// source file, copy fresh / overwrite-via-versions; for each source
/// directory, mkdir the dest counterpart. Records every relative
/// path encountered in `seen` so the caller can detect deletions
/// afterwards.
fn sync_source_into_dest(
    src_root: &Path,
    dest_root: &Path,
    versions_subroot: &Path,
    excludes: &GlobSet,
    timestamp: &str,
    seen: &mut HashSet<PathBuf>,
    stats: &mut MirrorStats,
) -> Result<()> {
    for entry in WalkDir::new(src_root).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("walking source: {e}");
                continue;
            }
        };
        let src_path = entry.path();
        if src_path == src_root {
            continue;
        }

        let rel = src_path
            .strip_prefix(src_root)
            .expect("walkdir entry must be under root");

        // Skip excludes (match on the relative path).
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if excludes.is_match(&rel_str) {
            continue;
        }

        // Refuse to copy from a `.versions` directory inside the source
        // (defensive — backup() already refuses one at the root, but
        // nested ones could in theory show up via symlinks etc.).
        if rel
            .components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new(VERSIONS_DIR))
        {
            continue;
        }

        let dest_path = dest_root.join(rel);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest_path).with_context(|| {
                format!("creating directory `{}`", dest_path.display())
            })?;
        } else if entry.file_type().is_file() {
            seen.insert(rel.to_path_buf());

            let src_meta = entry.metadata().with_context(|| {
                format!("stat-ing source file `{}`", src_path.display())
            })?;
            stats.bytes_total = stats.bytes_total.saturating_add(src_meta.len());

            match std::fs::metadata(&dest_path) {
                Ok(dest_meta) if files_match(&src_meta, &dest_meta) => {
                    stats.unchanged_files += 1;
                }
                Ok(_dest_meta) => {
                    // Overwrite: archive the old, then copy the new.
                    archive_to_versions(&dest_path, rel, versions_subroot, timestamp)?;
                    let bytes = copy_file_atomic(src_path, &dest_path)?;
                    stats.updated_files += 1;
                    stats.bytes_copied = stats.bytes_copied.saturating_add(bytes);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // New file → straight copy.
                    if let Some(parent) = dest_path.parent() {
                        std::fs::create_dir_all(parent).with_context(|| {
                            format!("creating parent of `{}`", dest_path.display())
                        })?;
                    }
                    let bytes = copy_file_atomic(src_path, &dest_path)?;
                    stats.new_files += 1;
                    stats.bytes_copied = stats.bytes_copied.saturating_add(bytes);
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "stat-ing dest file `{}`: {e}",
                        dest_path.display()
                    ));
                }
            }
        }
        // Symlinks / other entry types: ignored for v1.
    }
    Ok(())
}

/// Walk `dest_root`, find files that the source no longer carries
/// (i.e. not in `seen`), and move them into `.versions/` — same
/// archiving rule as overwrites, applied to deletions.
fn evict_files_missing_from_source(
    dest_root: &Path,
    versions_subroot: &Path,
    seen: &HashSet<PathBuf>,
    timestamp: &str,
    stats: &mut MirrorStats,
) -> Result<()> {
    // The dest_root contains the canonical current state. `.versions/`
    // doesn't live under dest_root (it sits at versions_subroot one
    // level up), so we don't need to filter it out here.
    let mut to_archive: Vec<(PathBuf, PathBuf)> = Vec::new();

    for entry in WalkDir::new(dest_root).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("walking dest: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let dest_path = entry.path();
        let rel = dest_path
            .strip_prefix(dest_root)
            .expect("walkdir entry must be under dest_root");
        if !seen.contains(rel) {
            to_archive.push((dest_path.to_path_buf(), rel.to_path_buf()));
        }
    }

    for (dest_path, rel) in to_archive {
        archive_to_versions(&dest_path, &rel, versions_subroot, timestamp)?;
        stats.deleted_files += 1;
    }
    Ok(())
}

/// `<dest_path>` is the canonical file we want to archive. Move it to
/// `<versions_subroot>/<rel parent>/<basename>-<ts>.<ext>`.
fn archive_to_versions(
    dest_path: &Path,
    rel: &Path,
    versions_subroot: &Path,
    timestamp: &str,
) -> Result<()> {
    let rel_parent = rel.parent().unwrap_or_else(|| Path::new(""));
    let versions_parent = versions_subroot.join(rel_parent);
    std::fs::create_dir_all(&versions_parent).with_context(|| {
        format!("creating versions parent `{}`", versions_parent.display())
    })?;

    let file_name = rel
        .file_name()
        .ok_or_else(|| {
            anyhow::anyhow!("cannot derive a file name from `{}`", rel.display())
        })?
        .to_string_lossy()
        .into_owned();
    let versioned_name = versioned_basename(&file_name, timestamp);
    let archived_path = versions_parent.join(&versioned_name);

    debug!(
        from = %dest_path.display(),
        to = %archived_path.display(),
        "archiving previous version"
    );

    // Same-volume rename is atomic. If the destination already exists
    // (two backups in the same second, exotic), we add a counter
    // suffix to disambiguate.
    let mut final_path = archived_path.clone();
    let mut suffix = 0u32;
    while final_path.exists() {
        suffix += 1;
        final_path = versions_parent.join(format!("{versioned_name}.{suffix}"));
    }

    std::fs::rename(dest_path, &final_path).with_context(|| {
        format!(
            "archiving `{}` → `{}`",
            dest_path.display(),
            final_path.display()
        )
    })?;
    Ok(())
}

/// Inject `-<ts>` between the file stem and extension.
/// `famille.jpg` + `2026-05-14-153022` → `famille-2026-05-14-153022.jpg`.
/// Extensionless files: `README` → `README-2026-05-14-153022`.
fn versioned_basename(name: &str, timestamp: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{timestamp}.{ext}"),
        _ => format!("{name}-{timestamp}"),
    }
}

/// Filesystem-safe timestamp without colons (Windows hates them).
fn version_timestamp() -> String {
    let now = jiff::Zoned::now();
    // YYYY-MM-DD-HHMMSS, UTC-equivalent ordering across timezones.
    now.strftime("%Y-%m-%d-%H%M%S").to_string()
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn build_exclude_set(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let glob = Glob::new(pat).with_context(|| format!("compiling exclude glob `{pat}`"))?;
        builder.add(glob);
    }
    builder.build().context("building exclude glob set")
}

fn files_match(src: &std::fs::Metadata, dest: &std::fs::Metadata) -> bool {
    if src.len() != dest.len() {
        return false;
    }
    // mtime comparison: only consider equal at second resolution to
    // avoid spurious "changed" matches caused by filesystems that
    // round timestamps differently (FAT32, network shares).
    let s = src.modified().ok();
    let d = dest.modified().ok();
    match (s, d) {
        (Some(s), Some(d)) => {
            // Walk back to UNIX_EPOCH and compare seconds.
            let s_secs = s
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let d_secs = d
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            s_secs == d_secs
        }
        _ => false,
    }
}

/// Copy `src` to `dest` via a temp file in the same directory, then
/// rename in place. This protects against partial-write corruption if
/// the process dies mid-copy.
fn copy_file_atomic(src: &Path, dest: &Path) -> Result<u64> {
    let parent = dest.parent().ok_or_else(|| {
        anyhow::anyhow!("dest path `{}` has no parent dir", dest.display())
    })?;
    std::fs::create_dir_all(parent)?;

    let file_name = dest
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("dest path `{}` has no file name", dest.display()))?;
    let mut tmp_name = std::ffi::OsString::from(".kovre-mirror-tmp-");
    tmp_name.push(file_name);
    let tmp_path = parent.join(&tmp_name);

    let bytes = std::fs::copy(src, &tmp_path)
        .with_context(|| format!("copy `{}` → `{}`", src.display(), tmp_path.display()))?;
    std::fs::rename(&tmp_path, dest)
        .with_context(|| format!("rename `{}` → `{}`", tmp_path.display(), dest.display()))?;
    Ok(bytes)
}

// ---------------------------------------------------------------------
// Retention (prune .versions/)
// ---------------------------------------------------------------------

/// Group every entry in `versions_root` by `(parent_dir, canonical_stem)`
/// and keep the `keep` most recent per group, deleting the rest.
fn prune_versions(versions_root: &Path, keep: usize) -> Result<RetentionOutcome> {
    use std::collections::BTreeMap;

    // Key = (parent dir, canonical basename — i.e. with the `-<ts>`
    // suffix stripped). Value = list of (timestamp string, full path).
    let mut groups: BTreeMap<(PathBuf, String), Vec<(String, PathBuf)>> = BTreeMap::new();

    for entry in WalkDir::new(versions_root).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("walking versions: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let (canonical, ts) = match parse_versioned_name(&name) {
            Some(parts) => parts,
            None => continue, // shouldn't happen for files we wrote, but skip defensively
        };
        let parent = path.parent().unwrap_or(versions_root).to_path_buf();
        groups
            .entry((parent, canonical))
            .or_default()
            .push((ts, path.to_path_buf()));
    }

    let mut kept = 0usize;
    let mut forgotten = 0usize;

    for (_key, mut versions) in groups {
        // Sort by timestamp descending (most recent first).
        versions.sort_by(|a, b| b.0.cmp(&a.0));
        for (i, (_ts, path)) in versions.into_iter().enumerate() {
            if i < keep {
                kept += 1;
            } else {
                if let Err(e) = std::fs::remove_file(&path) {
                    warn!(path = %path.display(), "could not delete old version: {e}");
                } else {
                    forgotten += 1;
                }
            }
        }
    }

    Ok(RetentionOutcome { kept, forgotten })
}

/// Recover `(canonical_name, timestamp)` from a versioned file name.
/// Returns `None` if the name doesn't match the `<stem>-<ts>.<ext>` shape.
fn parse_versioned_name(name: &str) -> Option<(String, String)> {
    // The timestamp is 17 chars: `YYYY-MM-DD-HHMMSS`. Find the last
    // occurrence of `-YYYY-MM-DD-HHMMSS` before the optional `.ext`
    // (or end of string for extension-less files).
    let (head, ext) = match name.rsplit_once('.') {
        Some((h, e)) => (h, Some(e)),
        None => (name, None),
    };
    // head looks like `stem-YYYY-MM-DD-HHMMSS`. Find the suffix.
    if head.len() < 18 {
        return None;
    }
    let candidate_ts = &head[head.len() - 17..];
    if !looks_like_timestamp(candidate_ts) {
        return None;
    }
    // The char just before should be `-`.
    let separator_idx = head.len() - 18;
    if head.as_bytes().get(separator_idx) != Some(&b'-') {
        return None;
    }
    let stem = &head[..separator_idx];
    let canonical = match ext {
        Some(e) => format!("{stem}.{e}"),
        None => stem.to_string(),
    };
    Some((canonical, candidate_ts.to_string()))
}

fn looks_like_timestamp(s: &str) -> bool {
    // Pattern: YYYY-MM-DD-HHMMSS (17 chars)
    if s.len() != 17 {
        return false;
    }
    let bytes = s.as_bytes();
    let digit = |i: usize| bytes[i].is_ascii_digit();
    digit(0) && digit(1) && digit(2) && digit(3)
        && bytes[4] == b'-'
        && digit(5) && digit(6)
        && bytes[7] == b'-'
        && digit(8) && digit(9)
        && bytes[10] == b'-'
        && digit(11) && digit(12) && digit(13) && digit(14) && digit(15) && digit(16)
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BackendKind;
    use std::fs;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, MirrorEngine, PathBuf) {
        let workspace = TempDir::new().unwrap();
        let source = workspace.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let repo = workspace.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let engine = MirrorEngine::new(RepoConfig {
            path: repo,
            backend: BackendKind::Mirror,
            password_file: None,
        });
        (workspace, engine, source)
    }

    #[test]
    fn versioned_basename_injects_timestamp_before_ext() {
        assert_eq!(
            versioned_basename("famille.jpg", "2026-05-14-120000"),
            "famille-2026-05-14-120000.jpg"
        );
        assert_eq!(
            versioned_basename("README", "2026-05-14-120000"),
            "README-2026-05-14-120000"
        );
        assert_eq!(
            versioned_basename(".gitkeep", "2026-05-14-120000"),
            ".gitkeep-2026-05-14-120000"
        );
        assert_eq!(
            versioned_basename("archive.tar.gz", "2026-05-14-120000"),
            "archive.tar-2026-05-14-120000.gz"
        );
    }

    #[test]
    fn parse_versioned_name_roundtrips() {
        assert_eq!(
            parse_versioned_name("famille-2026-05-14-120000.jpg"),
            Some(("famille.jpg".to_string(), "2026-05-14-120000".to_string()))
        );
        assert_eq!(
            parse_versioned_name("README-2026-05-14-120000"),
            Some(("README".to_string(), "2026-05-14-120000".to_string()))
        );
        assert_eq!(parse_versioned_name("famille.jpg"), None); // no timestamp
        // Form-only validation — out-of-range numbers still parse, that's by design:
        // we only parse our own output, where the timestamps are well-formed.
        assert_eq!(parse_versioned_name("short.txt"), None); // too short to contain a ts
        assert_eq!(parse_versioned_name("no_dash_before20260101120000.txt"), None);
    }

    #[test]
    fn init_creates_destination_root() {
        let workspace = TempDir::new().unwrap();
        let target = workspace.path().join("deep").join("nested").join("repo");
        let engine = MirrorEngine::new(RepoConfig {
            path: target.clone(),
            backend: BackendKind::Mirror,
            password_file: None,
        });
        engine.init().unwrap();
        assert!(target.is_dir());
    }

    #[test]
    fn first_backup_copies_files_into_dest() {
        let (_ws, engine, source) = fixture();
        fs::write(source.join("hello.txt"), b"hi").unwrap();
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested").join("deep.txt"), b"deep").unwrap();

        let info = engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        let mirror_root = engine.job_root("job1").join("source");
        assert_eq!(fs::read(mirror_root.join("hello.txt")).unwrap(), b"hi");
        assert_eq!(
            fs::read(mirror_root.join("nested").join("deep.txt")).unwrap(),
            b"deep"
        );
        // No .versions/ yet — nothing was overwritten.
        let v = engine.versions_root("job1").join("source");
        assert!(!v.join("hello.txt").exists());
        // Snapshot summary is the file count totals.
        assert!(info.total_bytes_processed.unwrap_or(0) >= 6);
    }

    #[test]
    fn second_backup_archives_modified_files() {
        let (_ws, engine, source) = fixture();
        fs::write(source.join("file.txt"), b"v1").unwrap();
        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        // Force a noticeable mtime gap; some filesystems round.
        thread::sleep(Duration::from_millis(1100));
        fs::write(source.join("file.txt"), b"v2-content").unwrap();

        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        let canonical = engine.job_root("job1").join("source").join("file.txt");
        assert_eq!(fs::read(&canonical).unwrap(), b"v2-content");

        // .versions/source/ should now hold one archived copy.
        let versions_dir = engine.versions_root("job1").join("source");
        let archived: Vec<_> = fs::read_dir(&versions_dir).unwrap().collect();
        assert_eq!(archived.len(), 1);
        let archived_path = archived.into_iter().next().unwrap().unwrap().path();
        assert_eq!(fs::read(&archived_path).unwrap(), b"v1");
    }

    #[test]
    fn deleted_source_file_is_moved_to_versions() {
        let (_ws, engine, source) = fixture();
        fs::write(source.join("doomed.txt"), b"to be deleted").unwrap();
        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        fs::remove_file(source.join("doomed.txt")).unwrap();
        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        // Canonical version gone.
        assert!(!engine
            .job_root("job1")
            .join("source")
            .join("doomed.txt")
            .exists());
        // But preserved under .versions/.
        let versions_dir = engine.versions_root("job1").join("source");
        let archived: Vec<_> = fs::read_dir(&versions_dir).unwrap().collect();
        assert_eq!(archived.len(), 1);
        let archived_path = archived.into_iter().next().unwrap().unwrap().path();
        assert_eq!(fs::read(&archived_path).unwrap(), b"to be deleted");
    }

    #[test]
    fn unchanged_files_are_not_re_archived() {
        let (_ws, engine, source) = fixture();
        fs::write(source.join("stable.txt"), b"unchanging").unwrap();
        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();
        // Second backup: nothing changed → no .versions entry, no new
        // file written.
        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        let versions_dir = engine.versions_root("job1").join("source");
        if versions_dir.exists() {
            let entries: Vec<_> = fs::read_dir(&versions_dir).unwrap().collect();
            assert_eq!(entries.len(), 0, "no archived versions expected");
        }
    }

    #[test]
    fn excludes_glob_skips_matching_files() {
        let (_ws, engine, source) = fixture();
        fs::write(source.join("keep.txt"), b"keep").unwrap();
        fs::write(source.join("scratch.tmp"), b"scratch").unwrap();

        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec!["**/*.tmp".into()],
                },
            )
            .unwrap();

        let mirror_root = engine.job_root("job1").join("source");
        assert!(mirror_root.join("keep.txt").is_file());
        assert!(!mirror_root.join("scratch.tmp").exists());
    }

    #[test]
    fn source_with_dot_versions_at_root_is_rejected() {
        let (_ws, engine, source) = fixture();
        fs::create_dir_all(source.join(".versions")).unwrap();
        let err = engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(".versions"), "got: {msg}");
    }

    #[test]
    fn retention_keep_versions_prunes_older_archives() {
        let (_ws, engine, source) = fixture();
        // Three rounds → two archived versions (the first round writes,
        // subsequent rounds archive the previous canonical).
        for i in 1..=3 {
            fs::write(source.join("file.txt"), format!("v{i}")).unwrap();
            engine
                .backup(
                    "job1",
                    BackupSource {
                        paths: vec![source.clone()],
                        excludes: vec![],
                    },
                )
                .unwrap();
            thread::sleep(Duration::from_millis(1100));
        }
        let versions_dir = engine.versions_root("job1").join("source");
        let archived_before = fs::read_dir(&versions_dir).unwrap().count();
        assert_eq!(archived_before, 2, "expected 2 archived versions");

        let outcome = engine
            .apply_retention(
                "job1",
                &Retention {
                    keep_versions: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(outcome.kept, 1);
        assert_eq!(outcome.forgotten, 1);

        let archived_after = fs::read_dir(&versions_dir).unwrap().count();
        assert_eq!(archived_after, 1);
    }

    #[test]
    fn retention_without_keep_versions_is_a_noop() {
        let (_ws, engine, _source) = fixture();
        let outcome = engine
            .apply_retention("job1", &Retention::default())
            .unwrap();
        assert_eq!(outcome.kept, 0);
        assert_eq!(outcome.forgotten, 0);
    }

    #[test]
    fn list_snapshots_returns_empty_for_mirror() {
        let (_ws, engine, _source) = fixture();
        assert!(engine.list_snapshots("anything").unwrap().is_empty());
    }
}
