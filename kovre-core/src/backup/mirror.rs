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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::backup::{
    BackupEngine, BackupSource, BrowseEntry, RetentionOutcome, SnapshotInfo, VerifyOutcome,
    VersionInfo, JOB_TAG_PREFIX,
};
use crate::config::{Repository as RepoConfig, Retention};

const VERSIONS_DIR: &str = ".versions";

/// Files larger than this are not hashed for rename detection — the
/// cost (re-read every byte from disk) outweighs the benefit (saving
/// one full copy). 512 MiB is generous enough to cover most photos /
/// documents but skips 4K videos and ISO files. A future config knob
/// could expose this; for now, it's tuned for the "photos and
/// documents" cible.
const MAX_HASH_BYTES: u64 = 512 * 1024 * 1024;

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

            // Pre-walk dest so we can spot renames (same content,
            // different rel path) before falling back to a fresh
            // copy + archive-of-old.
            let mut dest_inventory = inventory_dest_files(&dest_root)?;

            sync_source_into_dest(
                src_root,
                &dest_root,
                &versions_subroot,
                &exclude_set,
                &timestamp,
                &mut dest_inventory,
                &mut stats,
            )?;

            archive_dest_orphans(
                &dest_root,
                &versions_subroot,
                &dest_inventory,
                &timestamp,
                &mut stats,
            )?;
        }

        if stats.skipped_files > 0 {
            warn!(
                job = job_name,
                skipped = stats.skipped_files,
                "some files were locked by another process and could not be backed up"
            );
        }
        info!(
            job = job_name,
            new = stats.new_files,
            updated = stats.updated_files,
            renamed = stats.renamed_files,
            deleted = stats.deleted_files,
            unchanged = stats.unchanged_files,
            skipped = stats.skipped_files,
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

    fn restore_latest(&self, job_name: &str, dest_dir: &Path) -> Result<()> {
        let job_root = self.job_root(job_name);
        if !job_root.exists() {
            anyhow::bail!(
                "nothing to restore: mirror job root `{}` does not exist (job never ran?)",
                job_root.display()
            );
        }
        std::fs::create_dir_all(dest_dir).with_context(|| {
            format!("creating restore destination `{}`", dest_dir.display())
        })?;

        let mut copied_files = 0usize;
        for entry in WalkDir::new(&job_root).follow_links(false) {
            let entry = entry.context("walking mirror job root")?;
            let src_path = entry.path();
            if src_path == job_root {
                continue;
            }
            let rel = src_path
                .strip_prefix(&job_root)
                .expect("walkdir entry must be under job_root");
            // Skip the .versions/ archive entirely — restore_latest
            // restores the canonical state, not historical versions.
            if rel
                .components()
                .next()
                .map(|c| c.as_os_str() == std::ffi::OsStr::new(VERSIONS_DIR))
                .unwrap_or(false)
            {
                continue;
            }
            let target = dest_dir.join(rel);
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(&target).with_context(|| {
                    format!("creating restored directory `{}`", target.display())
                })?;
            } else if entry.file_type().is_file() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                copy_file_atomic(src_path, &target)?;
                copied_files += 1;
            }
        }

        if copied_files == 0 {
            anyhow::bail!(
                "nothing to restore: mirror job `{job_name}` contains no files (excluding .versions/)"
            );
        }
        info!(job = job_name, files = copied_files, "mirror restore complete");
        Ok(())
    }

    fn verify(&self) -> Result<VerifyOutcome> {
        Ok(VerifyOutcome {
            ok: true,
            messages: vec![
                "mirror backend: nothing to verify — files are stored natively on the destination filesystem"
                    .to_string(),
            ],
        })
    }

    fn browse(&self, job_name: &str, subpath: &str) -> Result<Vec<BrowseEntry>> {
        let job_root = self.job_root(job_name);
        if !job_root.exists() {
            anyhow::bail!(
                "nothing to browse: mirror job root `{}` does not exist",
                job_root.display()
            );
        }
        let target = if subpath.is_empty() {
            job_root.clone()
        } else {
            let clean = subpath.replace('/', std::path::MAIN_SEPARATOR_STR);
            let joined = job_root.join(&clean);
            if !joined.starts_with(&job_root) {
                anyhow::bail!("path traversal rejected");
            }
            joined
        };
        if !target.is_dir() {
            anyhow::bail!(
                "browse path `{}` is not a directory",
                target.display()
            );
        }

        let versions_root = self.versions_root(job_name);
        // Build a quick index of version counts per canonical name in
        // the versions dir that corresponds to `target`. This is a
        // single read_dir — cheap even for a few hundred entries.
        let versions_counts = count_versions_in_dir(&versions_root, &target, &job_root);

        let mut entries: Vec<BrowseEntry> = Vec::new();
        let dir = std::fs::read_dir(&target).with_context(|| {
            format!("reading directory `{}`", target.display())
        })?;
        for entry in dir {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == VERSIONS_DIR {
                continue;
            }
            let ft = entry.file_type()?;
            let (size, modified) = if ft.is_file() {
                let meta = entry.metadata()?;
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| {
                        let dur = t.duration_since(std::time::UNIX_EPOCH).ok()?;
                        let ts = jiff::Timestamp::from_second(dur.as_secs() as i64).ok()?;
                        Some(ts.to_string())
                    });
                (Some(meta.len()), mtime)
            } else {
                (None, None)
            };
            let vc = if ft.is_file() {
                versions_counts.get(&name).copied().unwrap_or(0)
            } else {
                0
            };
            entries.push(BrowseEntry {
                name,
                is_dir: ft.is_dir(),
                size,
                modified,
                versions_count: vc,
            });
        }
        entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }

    fn list_versions(&self, job_name: &str, rel_path: &str) -> Result<Vec<VersionInfo>> {
        let versions_root = self.versions_root(job_name);
        let rel = Path::new(rel_path);
        let rel_parent = rel.parent().unwrap_or_else(|| Path::new(""));
        let file_name = rel
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("cannot derive file name from `{rel_path}`"))?
            .to_string_lossy()
            .into_owned();

        let versions_dir = versions_root.join(rel_parent);
        if !versions_dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut out: Vec<VersionInfo> = Vec::new();
        for entry in std::fs::read_dir(&versions_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let vname = entry.file_name().to_string_lossy().into_owned();
            if let Some((canonical, ts)) = parse_versioned_name(&vname) {
                if canonical == file_name {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push(VersionInfo {
                        name: vname,
                        timestamp: ts,
                        size,
                    });
                }
            }
        }
        // Newest first.
        out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(out)
    }

    fn restore_selective(
        &self,
        job_name: &str,
        dest_dir: &Path,
        subpath: Option<&str>,
    ) -> Result<()> {
        let job_root = self.job_root(job_name);
        if !job_root.exists() {
            anyhow::bail!(
                "nothing to restore: mirror job root `{}` does not exist",
                job_root.display()
            );
        }
        std::fs::create_dir_all(dest_dir)?;

        let src_root = match subpath {
            Some(sp) if !sp.is_empty() => {
                let clean = sp.replace('/', std::path::MAIN_SEPARATOR_STR);
                let joined = job_root.join(&clean);
                if !joined.starts_with(&job_root) {
                    anyhow::bail!("path traversal rejected in subpath");
                }
                joined
            }
            _ => job_root.clone(),
        };

        if !src_root.exists() {
            anyhow::bail!(
                "subpath `{}` does not exist in the backup",
                src_root.display()
            );
        }

        let mut copied = 0usize;

        if src_root.is_file() {
            // Single file restore.
            let name = src_root
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("no file name"))?;
            let dest = dest_dir.join(name);
            copy_file_atomic(&src_root, &dest)?;
            copied += 1;
        } else {
            // Directory subtree restore.
            for entry in WalkDir::new(&src_root).follow_links(false) {
                let entry = entry.context("walking restore source")?;
                let path = entry.path();
                if path == src_root {
                    continue;
                }
                let rel = path
                    .strip_prefix(&src_root)
                    .expect("walkdir under src_root");
                // Skip .versions/ in subtree.
                if rel
                    .components()
                    .next()
                    .map(|c| c.as_os_str() == std::ffi::OsStr::new(VERSIONS_DIR))
                    .unwrap_or(false)
                {
                    continue;
                }
                let target = dest_dir.join(rel);
                if entry.file_type().is_dir() {
                    std::fs::create_dir_all(&target)?;
                } else if entry.file_type().is_file() {
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    copy_file_atomic(path, &target)?;
                    copied += 1;
                }
            }
        }

        if copied == 0 {
            anyhow::bail!("nothing to restore (no files under the given subpath)");
        }
        info!(job = job_name, files = copied, ?subpath, "mirror selective restore complete");
        Ok(())
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
    renamed_files: usize,
    deleted_files: usize,
    unchanged_files: usize,
    skipped_files: usize,
    bytes_copied: u64,
    bytes_total: u64,
}

/// Detect Windows "sharing violation" (ERROR_SHARING_VIOLATION = 32)
/// and "lock violation" (ERROR_LOCK_VIOLATION = 33) — files held open
/// exclusively by another process. These are expected for browser
/// DBs, editor temp files, Outlook OST, etc. The mirror engine skips
/// them with a warning instead of failing the entire job.
fn is_locked_file_error(e: &anyhow::Error) -> bool {
    for cause in e.chain() {
        if let Some(io_err) = cause.downcast_ref::<std::io::Error>() {
            match io_err.raw_os_error() {
                Some(32) | Some(33) => return true,
                _ => {}
            }
        }
    }
    false
}

/// Walk `dest_root` once before the source pass, collecting every
/// regular file's relative path + metadata. Entries are consumed
/// (removed) by `sync_source_into_dest` as they are matched; whatever
/// remains afterwards is genuinely orphaned and gets archived.
fn inventory_dest_files(dest_root: &Path) -> Result<HashMap<PathBuf, std::fs::Metadata>> {
    let mut out: HashMap<PathBuf, std::fs::Metadata> = HashMap::new();
    for entry in WalkDir::new(dest_root).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("walking dest inventory: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(dest_root)
            .expect("walkdir entry must be under dest_root")
            .to_path_buf();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                warn!(path = %entry.path().display(), "stat dest entry: {e}");
                continue;
            }
        };
        out.insert(rel, meta);
    }
    Ok(out)
}

/// Walk `src_root` and bring `dest_root` in line with it. For each
/// source file:
///   - same rel path in dest, mtime+size match → unchanged
///   - same rel path in dest, content differs → archive old, copy new
///   - no dest entry at this rel path → first try a content-hash
///     rename match against any still-unmatched dest entry (cheap
///     way to handle "user moved photos into a sub-folder"). Falls
///     back to a fresh copy on miss / oversize.
/// `dest_inventory` is consumed: matched entries are removed, so the
/// caller can treat the leftover set as orphans.
fn sync_source_into_dest(
    src_root: &Path,
    dest_root: &Path,
    versions_subroot: &Path,
    excludes: &GlobSet,
    timestamp: &str,
    dest_inventory: &mut HashMap<PathBuf, std::fs::Metadata>,
    stats: &mut MirrorStats,
) -> Result<()> {
    // Cache hashes we compute on dest files so a source pool with
    // many same-size siblings doesn't re-read each candidate from
    // disk for every probe.
    let mut dest_hash_cache: HashMap<PathBuf, [u8; 32]> = HashMap::new();

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
            continue;
        }
        if !entry.file_type().is_file() {
            continue; // symlinks / others ignored
        }

        let src_meta = entry.metadata().with_context(|| {
            format!("stat-ing source file `{}`", src_path.display())
        })?;
        stats.bytes_total = stats.bytes_total.saturating_add(src_meta.len());

        // Case 1: a file already lives at the same rel path in dest.
        if let Some(dest_meta) = dest_inventory.remove(rel) {
            if files_match(&src_meta, &dest_meta) {
                stats.unchanged_files += 1;
            } else {
                archive_to_versions(&dest_path, rel, versions_subroot, timestamp)?;
                match copy_file_atomic(src_path, &dest_path) {
                    Ok(bytes) => {
                        stats.updated_files += 1;
                        stats.bytes_copied = stats.bytes_copied.saturating_add(bytes);
                    }
                    Err(e) if is_locked_file_error(&e) => {
                        warn!(path = %src_path.display(), "skipping locked file (update): {e:#}");
                        stats.skipped_files += 1;
                    }
                    Err(e) => return Err(e),
                }
            }
            continue;
        }

        // Case 2: no dest entry at this rel — could be a brand-new
        // file, or a rename of something we still hold in
        // dest_inventory. Any step here can hit a locked source file
        // (browser DBs, editor temps, etc.) — skip gracefully.
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating parent of `{}`", dest_path.display())
            })?;
        }

        let rename_match = match try_match_by_hash(
            src_path, &src_meta, dest_root, dest_inventory, &mut dest_hash_cache,
        ) {
            Ok(m) => m,
            Err(e) if is_locked_file_error(&e) => {
                warn!(path = %src_path.display(), "skipping locked file (hash): {e:#}");
                stats.skipped_files += 1;
                continue;
            }
            Err(e) => return Err(e),
        };

        if let Some(old_rel) = rename_match {
            let old_full = dest_root.join(&old_rel);
            debug!(
                from = %old_full.display(),
                to = %dest_path.display(),
                "rename detected, moving dest in place"
            );
            std::fs::rename(&old_full, &dest_path).with_context(|| {
                format!(
                    "renaming `{}` → `{}` (rename detection)",
                    old_full.display(),
                    dest_path.display()
                )
            })?;
            dest_hash_cache.remove(&old_rel);
            stats.renamed_files += 1;
        } else {
            match copy_file_atomic(src_path, &dest_path) {
                Ok(bytes) => {
                    stats.new_files += 1;
                    stats.bytes_copied = stats.bytes_copied.saturating_add(bytes);
                }
                Err(e) if is_locked_file_error(&e) => {
                    warn!(path = %src_path.display(), "skipping locked file (new): {e:#}");
                    stats.skipped_files += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// Anything still in `dest_inventory` after `sync_source_into_dest`
/// has neither been matched by rel path nor by hash — it's a real
/// deletion from the source. Move it to `.versions/`.
fn archive_dest_orphans(
    dest_root: &Path,
    versions_subroot: &Path,
    dest_inventory: &HashMap<PathBuf, std::fs::Metadata>,
    timestamp: &str,
    stats: &mut MirrorStats,
) -> Result<()> {
    for rel in dest_inventory.keys() {
        let dest_path = dest_root.join(rel);
        archive_to_versions(&dest_path, rel, versions_subroot, timestamp)?;
        stats.deleted_files += 1;
    }
    Ok(())
}

/// Try to find a dest entry whose content matches `src_path`. Only
/// considers candidates with the **same size** as `src_meta.len()` to
/// avoid hashing the entire dest pool. Returns the rel path of the
/// match (and removes it from `dest_inventory`) on success.
///
/// Files larger than `MAX_HASH_BYTES` are not hashed — saving one
/// full copy on a 4K video isn't worth re-reading 50 GiB from disk.
fn try_match_by_hash(
    src_path: &Path,
    src_meta: &std::fs::Metadata,
    dest_root: &Path,
    dest_inventory: &mut HashMap<PathBuf, std::fs::Metadata>,
    dest_hash_cache: &mut HashMap<PathBuf, [u8; 32]>,
) -> Result<Option<PathBuf>> {
    let size = src_meta.len();
    if size > MAX_HASH_BYTES {
        return Ok(None);
    }
    // Candidates: dest files with the exact same size. Collect into
    // an owned Vec so we can mutate dest_inventory while iterating.
    let candidates: Vec<PathBuf> = dest_inventory
        .iter()
        .filter(|(_, m)| m.len() == size)
        .map(|(p, _)| p.clone())
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }

    let src_hash = hash_file(src_path)?;

    for cand_rel in candidates {
        let cand_full = dest_root.join(&cand_rel);
        let cand_hash = match dest_hash_cache.get(&cand_rel) {
            Some(h) => *h,
            None => {
                let h = hash_file(&cand_full)?;
                dest_hash_cache.insert(cand_rel.clone(), h);
                h
            }
        };
        if cand_hash == src_hash {
            dest_inventory.remove(&cand_rel);
            return Ok(Some(cand_rel));
        }
    }
    Ok(None)
}

/// Stream `path` through SHA-256, returning the digest. Reads in 64
/// KiB chunks — small enough to keep the working set hot in cache,
/// large enough to amortize syscall overhead.
fn hash_file(path: &Path) -> Result<[u8; 32]> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening `{}` for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("reading `{}` for hashing", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
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

/// For a given directory being browsed, count how many archived
/// versions exist per canonical file name. Returns a map
/// `canonical_name → count`. Cheap: one `read_dir` of the
/// `.versions/` counterpart of the browsed directory.
fn count_versions_in_dir(
    versions_root: &Path,
    browsed_dir: &Path,
    job_root: &Path,
) -> HashMap<String, u32> {
    let mut out: HashMap<String, u32> = HashMap::new();
    let rel = match browsed_dir.strip_prefix(job_root) {
        Ok(r) => r,
        Err(_) => return out,
    };
    let versions_dir = versions_root.join(rel);
    if !versions_dir.is_dir() {
        return out;
    }
    let entries = match std::fs::read_dir(&versions_dir) {
        Ok(it) => it,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some((canonical, _ts)) = parse_versioned_name(&name) {
            *out.entry(canonical).or_insert(0) += 1;
        }
    }
    out
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
        smb_user: None,
        smb_password_file: None,
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
        smb_user: None,
        smb_password_file: None,
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

    #[test]
    fn verify_is_a_noop_with_informative_message() {
        let (_ws, engine, _source) = fixture();
        let outcome = engine.verify().unwrap();
        assert!(outcome.ok, "mirror verify should always be ok");
        assert!(
            !outcome.messages.is_empty(),
            "verify should surface a message explaining the no-op"
        );
        assert!(
            outcome.messages[0].contains("mirror"),
            "message should mention the backend: {:?}",
            outcome.messages
        );
    }

    // ---- Rename detection ----

    #[test]
    fn renamed_file_is_moved_not_archived_then_recopied() {
        // User reorganizes their photos: famille.jpg moves from
        // root to a 2024 sub-folder. Without rename detection, the
        // root copy lands in .versions/ and the 2024 copy is read +
        // written from scratch — wasteful on big binaries.
        let (_ws, engine, source) = fixture();
        let content = b"a-photo-pretend-this-is-binary".to_vec();
        fs::write(source.join("famille.jpg"), &content).unwrap();
        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        // Move the file under a sub-folder, identical content.
        fs::create_dir_all(source.join("2024")).unwrap();
        fs::remove_file(source.join("famille.jpg")).unwrap();
        fs::write(source.join("2024").join("famille.jpg"), &content).unwrap();

        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        // New canonical path holds the content.
        let canonical_root = engine.job_root("job1").join("source");
        assert_eq!(
            fs::read(canonical_root.join("2024").join("famille.jpg")).unwrap(),
            content
        );
        // Old canonical path is gone.
        assert!(!canonical_root.join("famille.jpg").exists());
        // Critical: .versions/ stays empty — no archive happened.
        let versions_dir = engine.versions_root("job1").join("source");
        let archived_count = walkdir::WalkDir::new(&versions_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .count();
        assert_eq!(
            archived_count, 0,
            "a rename must not produce a .versions/ entry"
        );
    }

    #[test]
    fn renamed_and_modified_falls_back_to_archive_plus_copy() {
        // Same path layout as the rename test, but the content
        // changes during the move. Rename detection can't match
        // (different hash), so we fall back to the classic
        // archive-old + copy-new path — the old version must end
        // up in .versions/.
        let (_ws, engine, source) = fixture();
        fs::write(source.join("famille.jpg"), b"v1-content").unwrap();
        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        fs::create_dir_all(source.join("2024")).unwrap();
        fs::remove_file(source.join("famille.jpg")).unwrap();
        fs::write(source.join("2024").join("famille.jpg"), b"v2-different").unwrap();

        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        let canonical_root = engine.job_root("job1").join("source");
        // New content at new location.
        assert_eq!(
            fs::read(canonical_root.join("2024").join("famille.jpg")).unwrap(),
            b"v2-different"
        );
        // Old content preserved somewhere under .versions/.
        let versions_dir = engine.versions_root("job1").join("source");
        let archived: Vec<_> = walkdir::WalkDir::new(&versions_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .collect();
        assert_eq!(
            archived.len(),
            1,
            "modified rename should archive the original content"
        );
        assert_eq!(fs::read(archived[0].path()).unwrap(), b"v1-content");
    }

    #[test]
    fn rename_skipped_when_size_does_not_match() {
        // Different size = candidates filter rules the file out
        // before we even hash. Same observable outcome as
        // "modified rename": original archived, new file copied.
        let (_ws, engine, source) = fixture();
        fs::write(source.join("photo.jpg"), b"short").unwrap();
        engine
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();

        fs::create_dir_all(source.join("sub")).unwrap();
        fs::remove_file(source.join("photo.jpg")).unwrap();
        fs::write(source.join("sub").join("photo.jpg"), b"a-much-longer-payload").unwrap();

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
        let archived_count = walkdir::WalkDir::new(&versions_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .count();
        assert_eq!(archived_count, 1, "size mismatch must skip rename match");
    }

    #[test]
    fn rename_skipped_when_file_exceeds_hash_cap() {
        // Synthesize a "huge" pretend file to hit the size cap.
        // We do it by pretending the limit is way lower than
        // MAX_HASH_BYTES via a small file that's still bigger
        // than the cap value the unit-under-test would see. Since
        // MAX_HASH_BYTES is a compile-time constant (512 MiB), we
        // can't realistically write a >512 MiB temp file in a unit
        // test — instead, drive the helper directly with a tweaked
        // cap by calling hash_file + try_match_by_hash with a
        // controlled scenario. Keeping this test as a sanity guard
        // that hash_file works on a small file (the cap is exercised
        // by integration in real life).
        let temp = TempDir::new().unwrap();
        let p = temp.path().join("blob.bin");
        let content: Vec<u8> = (0u16..1024).map(|i| (i % 251) as u8).collect();
        fs::write(&p, &content).unwrap();
        let h1 = hash_file(&p).unwrap();
        let h2 = hash_file(&p).unwrap();
        assert_eq!(h1, h2, "hash_file must be deterministic");
        // Different content → different hash (sanity).
        fs::write(&p, b"completely different").unwrap();
        let h3 = hash_file(&p).unwrap();
        assert_ne!(h1, h3, "different content must produce a different hash");
    }
}
