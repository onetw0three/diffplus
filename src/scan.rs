//! Disk-backed, bounded directory and archive traversal.

use crate::model::{Content, Entry};
use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, Cursor, Read, Write},
    path::{Path, PathBuf},
};
use tar::Archive as TarArchive;
use zip::ZipArchive;

const SAMPLE_SIZE: usize = 64 * 1024;
const INITIAL_SAMPLE_CAPACITY: usize = 4 * 1024;
const IO_BUFFER_SIZE: usize = 256 * 1024;
const HASH_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct ArchiveLimits {
    pub(crate) max_file: u64,
    pub(crate) max_expanded: u64,
    pub(crate) max_depth: usize,
}

#[derive(Clone)]
pub(crate) struct ContentStore {
    root: PathBuf,
}

impl ContentStore {
    pub(crate) fn new(root: &Path) -> Result<Self> {
        let root = root.join("blobs");
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub(crate) fn stage_bytes(&self, logical: String, bytes: &[u8], max: u64) -> Result<Entry> {
        self.stage_reader(logical, Cursor::new(bytes), max, None)
    }

    fn stage_reader(
        &self,
        logical: String,
        mut reader: impl Read,
        max: u64,
        kind: Option<&'static str>,
    ) -> Result<Entry> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        let mut hash = Sha256::new();
        let mut sample = Vec::with_capacity(INITIAL_SAMPLE_CAPACITY);
        let mut size = 0_u64;
        let mut buffer = vec![0_u8; IO_BUFFER_SIZE];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            size = size
                .checked_add(read as u64)
                .context("content size overflow")?;
            if size > max {
                bail!("{logical} exceeds max-file-size");
            }
            if sample.len() < SAMPLE_SIZE {
                let take = read.min(SAMPLE_SIZE - sample.len());
                sample.extend_from_slice(&buffer[..take]);
            }
            hash.update(&buffer[..read]);
            temporary.write_all(&buffer[..read])?;
        }
        let digest = format!("{:x}", hash.finalize());
        let destination = self.root.join(&digest);
        if let Err(error) = temporary.persist_noclobber(&destination) {
            if error.error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.error.into());
            }
        }
        Ok(Entry {
            path: logical,
            content: Content::File(destination),
            digest,
            size,
            kind: kind.unwrap_or_else(|| classify_sample(&sample)),
        })
    }

    fn materialize(&self, entry: &Entry) -> Result<PathBuf> {
        if let Content::File(path) = &entry.content {
            return Ok(path.clone());
        }
        let destination = self.root.join(&entry.digest);
        if destination.exists() {
            return Ok(destination);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        std::io::copy(&mut entry.content.open()?, &mut temporary)?;
        if let Err(error) = temporary.persist_noclobber(&destination) {
            if error.error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.error.into());
            }
        }
        Ok(destination)
    }

    fn stage_link(&self, logical: String, target: &str, max: u64) -> Result<Entry> {
        self.stage_reader(
            logical,
            Cursor::new(target.as_bytes()),
            max,
            Some("symlink"),
        )
    }
}

pub(crate) fn collect_path(
    path: &Path,
    out: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    limits: ArchiveLimits,
    expanded: &mut u64,
) -> Result<()> {
    if archive_kind(path, path)?.is_some() {
        return collect_archive(path, path, "", out, store, limits, 1, expanded);
    }
    let logical = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let entry = reference_file(logical, path, limits.max_file)?;
    account_expanded(entry.size, limits.max_expanded, expanded, &entry.path)?;
    insert_unique(out, entry)
}

pub(crate) fn is_archive_file(path: &Path) -> Result<bool> {
    Ok(path.is_file() && archive_kind(path, path)?.is_some())
}

pub(crate) fn is_uncompressed_tar(path: &Path) -> Result<bool> {
    Ok(path.is_file() && matches!(archive_kind(path, path)?, Some(ArchiveKind::Tar)))
}

pub(crate) fn collect_dir(
    root: &Path,
    current: &Path,
    out: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    limits: ArchiveLimits,
    expanded: &mut u64,
) -> Result<()> {
    for item in std::fs::read_dir(current)? {
        let item = item?;
        let path = item.path();
        let relative = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        let file_type = item.file_type()?;
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path)?.to_string_lossy().into_owned();
            let entry = store.stage_link(relative, &target, limits.max_file)?;
            account_expanded(entry.size, limits.max_expanded, expanded, &entry.path)?;
            insert_unique(out, entry)?;
        } else if file_type.is_dir() {
            collect_dir(root, &path, out, store, limits, expanded)?;
        } else if archive_kind(&path, &path)?.is_some() {
            collect_archive(
                &path,
                &path,
                &format!("{relative}!/"),
                out,
                store,
                limits,
                1,
                expanded,
            )?;
        } else {
            let entry = reference_file(relative, &path, limits.max_file)?;
            account_expanded(entry.size, limits.max_expanded, expanded, &entry.path)?;
            insert_unique(out, entry)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_archive(
    content: &Path,
    source_name: &Path,
    prefix: &str,
    out: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    limits: ArchiveLimits,
    depth: usize,
    expanded: &mut u64,
) -> Result<()> {
    if depth > limits.max_depth {
        bail!(
            "archive recursion limit exceeded for {}",
            source_name.display()
        );
    }
    match archive_kind(content, source_name)? {
        Some(ArchiveKind::Zip) => collect_zip(content, prefix, out, store, limits, depth, expanded),
        Some(ArchiveKind::Tar) => {
            crate::progress::info(format!("streaming TAR input: {}", source_name.display()));
            collect_tar(
                BufReader::with_capacity(IO_BUFFER_SIZE, File::open(content)?),
                Some(content),
                prefix,
                out,
                store,
                limits,
                depth,
                expanded,
            )
        }
        Some(kind @ (ArchiveKind::Gzip | ArchiveKind::Bzip2 | ArchiveKind::Xz)) => {
            collect_compressed(
                content,
                source_name,
                prefix,
                kind,
                out,
                store,
                limits,
                depth,
                expanded,
            )
        }
        None => bail!("unsupported archive format: {}", source_name.display()),
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_zip(
    content: &Path,
    prefix: &str,
    out: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    limits: ArchiveLimits,
    depth: usize,
    expanded: &mut u64,
) -> Result<()> {
    let mut archive = ZipArchive::new(BufReader::with_capacity(
        IO_BUFFER_SIZE,
        File::open(content)?,
    ))
    .context("invalid ZIP archive")?;
    for index in 0..archive.len() {
        if index > 0 && index.is_multiple_of(500) {
            crate::progress::info(format!("ZIP progress: {index}/{} members", archive.len()));
        }
        let mut file = archive.by_index(index)?;
        if file.is_dir() {
            continue;
        }
        let name = safe_name(file.name())?;
        let logical = format!("{prefix}{name}");
        precheck_member(file.size(), &logical, limits, *expanded)?;
        let entry = store.stage_reader(logical, &mut file, limits.max_file, None)?;
        process_member(entry, out, store, limits, depth, expanded)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_tar<R: Read>(
    reader: R,
    backing: Option<&Path>,
    prefix: &str,
    out: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    limits: ArchiveLimits,
    depth: usize,
    expanded: &mut u64,
) -> Result<()> {
    let mut archive = TarArchive::new(reader);
    let mut count = 0_usize;
    for item in archive.entries()? {
        count += 1;
        if count.is_multiple_of(500) {
            crate::progress::info(format!("TAR progress: {count} members"));
        }
        let mut file = item.with_context(|| format!("reading TAR member #{count}"))?;
        let path = file
            .path()
            .with_context(|| format!("reading path for TAR member #{count}"))?;
        let name = safe_name(&path.to_string_lossy().replace('\\', "/"))?;
        let logical = format!("{prefix}{name}");
        let entry_type = file.header().entry_type();
        if entry_type.is_dir() {
            continue;
        }
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            let target = file
                .link_name()?
                .context("archive link has no target")?
                .to_string_lossy()
                .into_owned();
            let entry = store.stage_link(logical, &target, limits.max_file)?;
            account_expanded(entry.size, limits.max_expanded, expanded, &entry.path)?;
            insert_unique(out, entry)?;
            continue;
        }
        let size = file
            .header()
            .size()
            .with_context(|| format!("reading size for TAR member {logical}"))?;
        precheck_member(size, &logical, limits, *expanded)?;
        let entry = if let Some(backing) = backing.filter(|_| entry_type.is_file()) {
            let offset = file.raw_file_position();
            let (digest, sample, actual_size) =
                inspect_reader(&mut file, limits.max_file, &logical)?;
            if actual_size != size {
                bail!("TAR member size changed while reading {logical}");
            }
            Entry {
                path: logical,
                content: Content::Range {
                    source: backing.to_path_buf(),
                    offset,
                    size,
                },
                digest,
                size,
                kind: classify_sample(&sample),
            }
        } else {
            store.stage_reader(logical, &mut file, limits.max_file, None)?
        };
        process_member(entry, out, store, limits, depth, expanded)?;
    }
    crate::progress::info(format!("TAR scan complete: {count} members"));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_compressed(
    content: &Path,
    source_name: &Path,
    prefix: &str,
    kind: ArchiveKind,
    out: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    limits: ArchiveLimits,
    depth: usize,
    expanded: &mut u64,
) -> Result<()> {
    let input = BufReader::with_capacity(IO_BUFFER_SIZE, File::open(content)?);
    let logical = format!("{prefix}{}", decompressed_name(source_name));
    match kind {
        ArchiveKind::Gzip => collect_decoded(
            GzDecoder::new(input),
            logical,
            prefix,
            out,
            store,
            limits,
            depth,
            expanded,
        ),
        ArchiveKind::Bzip2 => collect_decoded(
            bzip2::read::BzDecoder::new(input),
            logical,
            prefix,
            out,
            store,
            limits,
            depth,
            expanded,
        ),
        ArchiveKind::Xz => collect_decoded(
            xz2::read::XzDecoder::new(input),
            logical,
            prefix,
            out,
            store,
            limits,
            depth,
            expanded,
        ),
        ArchiveKind::Zip | ArchiveKind::Tar => unreachable!(),
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_decoded(
    mut reader: impl Read,
    logical: String,
    prefix: &str,
    out: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    limits: ArchiveLimits,
    depth: usize,
    expanded: &mut u64,
) -> Result<()> {
    let mut header = Vec::with_capacity(512);
    reader.by_ref().take(512).read_to_end(&mut header)?;
    let decoded = Cursor::new(header.clone()).chain(reader);
    if is_tar(&header) {
        return collect_tar(decoded, None, prefix, out, store, limits, depth, expanded);
    }
    let remaining = limits.max_expanded.saturating_sub(*expanded);
    let member_limit = limits.max_file.min(remaining);
    let entry = store.stage_reader(logical, decoded, member_limit, None)?;
    account_expanded(entry.size, limits.max_expanded, expanded, &entry.path)?;
    if archive_kind_entry(&entry)?.is_some() {
        let content = store.materialize(&entry)?;
        return collect_archive(
            &content,
            Path::new(&entry.path),
            prefix,
            out,
            store,
            limits,
            depth,
            expanded,
        );
    }
    if entry.size > limits.max_file {
        bail!("decompressed file {} exceeds max-file-size", entry.path);
    }
    insert_unique(out, entry)
}

fn process_member(
    entry: Entry,
    out: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    limits: ArchiveLimits,
    depth: usize,
    expanded: &mut u64,
) -> Result<()> {
    account_expanded(entry.size, limits.max_expanded, expanded, &entry.path)?;
    if depth < limits.max_depth && archive_kind_entry(&entry)?.is_some() {
        let content = store.materialize(&entry)?;
        let prefix = format!("{}!/", entry.path);
        collect_archive(
            &content,
            Path::new(&entry.path),
            &prefix,
            out,
            store,
            limits,
            depth + 1,
            expanded,
        )
        .with_context(|| format!("expanding nested archive {}", entry.path))
    } else {
        insert_unique(out, entry)
    }
}

fn reference_file(logical: String, path: &Path, max: u64) -> Result<Entry> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > max {
        bail!("{logical} exceeds max-file-size");
    }
    let (digest, sample, size) = inspect_reader(File::open(path)?, max, &logical)?;
    Ok(Entry {
        path: logical,
        content: Content::File(path.to_path_buf()),
        digest,
        size,
        kind: classify_sample(&sample),
    })
}

fn inspect_reader(
    mut reader: impl Read,
    max: u64,
    logical: &str,
) -> Result<(String, Vec<u8>, u64)> {
    let mut hash = Sha256::new();
    let mut sample = Vec::with_capacity(INITIAL_SAMPLE_CAPACITY);
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; IO_BUFFER_SIZE];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .context("content size overflow")?;
        if size > max {
            bail!("{logical} exceeds max-file-size");
        }
        if sample.len() < SAMPLE_SIZE {
            let take = read.min(SAMPLE_SIZE - sample.len());
            sample.extend_from_slice(&buffer[..take]);
        }
        hash.update(&buffer[..read]);
    }
    Ok((format!("{:x}", hash.finalize()), sample, size))
}

fn classify_sample(sample: &[u8]) -> &'static str {
    if sample.contains(&0) {
        return "binary";
    }
    match std::str::from_utf8(sample) {
        Ok(_) => "text",
        Err(error) if error.error_len().is_none() => "text",
        Err(_) => "binary",
    }
}

fn insert_unique(out: &mut BTreeMap<String, Entry>, entry: Entry) -> Result<()> {
    if out.contains_key(&entry.path) {
        bail!("duplicate archive member: {}", entry.path);
    }
    out.insert(entry.path.clone(), entry);
    Ok(())
}

fn precheck_member(size: u64, name: &str, limits: ArchiveLimits, expanded: u64) -> Result<()> {
    if size > limits.max_file {
        bail!("archive member {name} exceeds max-file-size");
    }
    if expanded.saturating_add(size) > limits.max_expanded {
        bail!("archive expanded-size limit exceeded before reading {name}");
    }
    Ok(())
}

fn account_expanded(size: u64, limit: u64, expanded: &mut u64, name: &str) -> Result<()> {
    if size > limit.saturating_sub(*expanded) {
        bail!("expanded-size limit exceeded while reading {name}");
    }
    *expanded += size;
    Ok(())
}

#[derive(Clone, Copy)]
enum ArchiveKind {
    Zip,
    Tar,
    Gzip,
    Bzip2,
    Xz,
}

fn archive_kind(content: &Path, source_name: &Path) -> Result<Option<ArchiveKind>> {
    let mut file = File::open(content)?;
    let mut header = [0_u8; 512];
    let size = file.read(&mut header)?;
    Ok(detect_archive(&header[..size], source_name))
}

fn archive_kind_entry(entry: &Entry) -> Result<Option<ArchiveKind>> {
    let mut reader = entry.content.open()?;
    let mut header = [0_u8; 512];
    let size = reader.read(&mut header)?;
    Ok(detect_archive(&header[..size], Path::new(&entry.path)))
}

fn detect_archive(bytes: &[u8], source_name: &Path) -> Option<ArchiveKind> {
    let lower = source_name.to_string_lossy().to_ascii_lowercase();
    if bytes.starts_with(b"PK\x03\x04") || lower.ends_with(".zip") || lower.ends_with(".jar") {
        Some(ArchiveKind::Zip)
    } else if is_tar(bytes) || lower.ends_with(".tar") {
        Some(ArchiveKind::Tar)
    } else if bytes.starts_with(b"\x1f\x8b") || lower.ends_with(".gz") || lower.ends_with(".tgz") {
        Some(ArchiveKind::Gzip)
    } else if bytes.starts_with(b"BZh") || lower.ends_with(".bz2") {
        Some(ArchiveKind::Bzip2)
    } else if bytes.starts_with(b"\xfd7zXZ\0") || lower.ends_with(".xz") {
        Some(ArchiveKind::Xz)
    } else {
        None
    }
}

fn is_tar(bytes: &[u8]) -> bool {
    bytes.get(257..262) == Some(b"ustar")
}

fn decompressed_name(source: &Path) -> String {
    let name = source.file_name().unwrap_or_default().to_string_lossy();
    for extension in [".gz", ".bz2", ".xz"] {
        if let Some(stripped) = name.strip_suffix(extension) {
            return if stripped.is_empty() {
                "decompressed".to_string()
            } else {
                stripped.to_string()
            };
        }
    }
    if let Some(stripped) = name.strip_suffix(".tgz") {
        return format!("{stripped}.tar");
    }
    format!("{name}.decompressed")
}

fn safe_name(name: &str) -> Result<String> {
    let normalized = name.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.split('/').any(|component| component == "..")
    {
        bail!("unsafe archive path: {name}");
    }
    let normalized = normalized.trim_start_matches("./").to_string();
    if normalized.is_empty() {
        bail!("unsafe archive path: {name}");
    }
    Ok(normalized)
}

pub(crate) fn sha(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}

pub(crate) fn sha_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_SIZE];
    let mut processed = 0_u64;
    let mut next_report = 256 * 1024 * 1024_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        processed += read as u64;
        if processed >= next_report {
            crate::progress::info(format!(
                "hashing {}: {} MiB",
                path.display(),
                processed / (1024 * 1024)
            ));
            next_report = next_report.saturating_add(256 * 1024 * 1024);
        }
    }
    crate::progress::info(format!(
        "hash complete: {} ({} MiB)",
        path.display(),
        processed / (1024 * 1024)
    ));
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn strip_common_top_level(entries: &mut BTreeMap<String, Entry>) {
    let Some(first) = entries
        .keys()
        .next()
        .and_then(|path| path.split('/').next())
        .map(str::to_string)
    else {
        return;
    };
    if entries
        .keys()
        .all(|path| path.starts_with(&(first.clone() + "/")))
    {
        for (path, mut entry) in std::mem::take(entries) {
            let path = path[first.len() + 1..].to_string();
            entry.path.clone_from(&path);
            entries.insert(path, entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(depth: usize) -> ArchiveLimits {
        ArchiveLimits {
            max_file: 4096,
            max_expanded: 16384,
            max_depth: depth,
        }
    }

    fn zip_with_file(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut output);
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
            writer.finish().unwrap();
        }
        output.into_inner()
    }

    fn scan_bytes(
        name: &str,
        bytes: &[u8],
        depth: usize,
    ) -> (tempfile::TempDir, BTreeMap<String, Entry>) {
        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join(name);
        std::fs::write(&input, bytes).unwrap();
        let store = ContentStore::new(temporary.path()).unwrap();
        let mut entries = BTreeMap::new();
        let mut expanded = 0;
        collect_path(&input, &mut entries, &store, limits(depth), &mut expanded).unwrap();
        for entry in entries.values() {
            assert!(entry.content.open().is_ok());
        }
        (temporary, entries)
    }

    #[test]
    fn rejects_traversal() {
        assert!(safe_name("../x").is_err());
        assert!(safe_name("a/../../x").is_err());
    }

    #[test]
    fn gzip_does_not_have_to_contain_tar() {
        let mut encoded = Vec::new();
        {
            let mut encoder =
                flate2::write::GzEncoder::new(&mut encoded, flate2::Compression::default());
            encoder.write_all(b"plain text\n").unwrap();
            encoder.finish().unwrap();
        }
        let (_temporary, entries) = scan_bytes("notes.txt.gz", &encoded, 1);
        assert_eq!(
            entries["notes.txt"].content.read().unwrap(),
            b"plain text\n"
        );
    }

    #[test]
    fn compressed_tar_is_one_archive_level() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(6);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "inside.txt", Cursor::new(b"inside"))
                .unwrap();
            builder.finish().unwrap();
        }
        let mut encoded = Vec::new();
        {
            let mut encoder =
                flate2::write::GzEncoder::new(&mut encoded, flate2::Compression::default());
            encoder.write_all(&tar_bytes).unwrap();
            encoder.finish().unwrap();
        }
        let (_temporary, entries) = scan_bytes("bundle.tar.gz", &encoded, 1);
        assert_eq!(entries["inside.txt"].content.read().unwrap(), b"inside");
    }

    #[test]
    fn uncompressed_tar_members_reference_source_ranges() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(7);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "range.txt", Cursor::new(b"content"))
                .unwrap();
            builder.finish().unwrap();
        }
        let (temporary, entries) = scan_bytes("bundle.tar", &tar_bytes, 1);
        assert!(matches!(
            entries["range.txt"].content,
            Content::Range { .. }
        ));
        assert_eq!(entries["range.txt"].content.read().unwrap(), b"content");
        assert!(std::fs::read_dir(temporary.path().join("blobs"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn nested_archive_in_tar_is_materialized_only_when_requested() {
        let inner = zip_with_file("inside.txt", b"inside");
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(inner.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "inner.zip", Cursor::new(inner))
                .unwrap();
            builder.finish().unwrap();
        }
        let (_temporary, entries) = scan_bytes("bundle.tar", &tar_bytes, 2);
        assert_eq!(
            entries["inner.zip!/inside.txt"].content.read().unwrap(),
            b"inside"
        );
    }

    #[test]
    fn depth_one_preserves_embedded_archives() {
        let inner = zip_with_file("inside.txt", b"inside");
        let outer = zip_with_file("inner.zip", &inner);
        let (_temporary, entries) = scan_bytes("outer.zip", &outer, 1);
        assert!(entries.contains_key("inner.zip"));
        assert!(!entries.contains_key("inner.zip!/inside.txt"));
    }

    #[test]
    fn depth_two_expands_one_embedded_archive() {
        let inner = zip_with_file("inside.txt", b"inside");
        let outer = zip_with_file("inner.zip", &inner);
        let (_temporary, entries) = scan_bytes("outer.zip", &outer, 2);
        assert!(entries.contains_key("inner.zip!/inside.txt"));
        assert!(!entries.contains_key("inner.zip"));
    }

    #[test]
    fn directory_files_are_referenced_in_place() {
        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join("input");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&input).unwrap();
        std::fs::write(input.join("hello.txt"), "hello\n").unwrap();
        let store = ContentStore::new(&workspace).unwrap();
        let mut entries = BTreeMap::new();
        collect_dir(&input, &input, &mut entries, &store, limits(1), &mut 0).unwrap();
        assert_eq!(
            entries["hello.txt"].content,
            Content::File(input.join("hello.txt"))
        );
        assert!(std::fs::read_dir(workspace.join("blobs"))
            .unwrap()
            .next()
            .is_none());
    }
}
