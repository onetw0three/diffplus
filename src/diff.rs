use anyhow::Result;
use std::{collections::BTreeSet, path::Path};

use crate::model::{Artifact, ArtifactInfo, Entry, Manifest, ManifestEntry, Stats};

pub(crate) fn compare(
    old: &Artifact,
    new: &Artifact,
    out: &Path,
    context: usize,
) -> Result<(Manifest, String)> {
    let mut stats = Stats::default();
    let mut entries = Vec::new();
    for pair in pair_entries(old, new) {
        let old_path = pair.old_path;
        let new_path = pair.new_path;
        let path = new_path.or(old_path).expect("entry pair has a path");
        let a = old_path.and_then(|path| old.entries.get(path));
        let b = new_path.and_then(|path| new.entries.get(path));
        let renamed = old_path.zip(new_path).is_some_and(|(old, new)| old != new);
        if renamed {
            stats.renamed += 1;
        }
        let (status, kind) = match (a, b) {
            (Some(x), Some(y)) if x.digest == y.digest => {
                if renamed {
                    ("renamed", x.kind)
                } else {
                    stats.unchanged += 1;
                    ("unchanged", x.kind)
                }
            }
            (Some(x), Some(y)) => {
                stats.modified += 1;
                (
                    "modified",
                    if x.kind == "text" && y.kind == "text" {
                        "text"
                    } else {
                        "binary"
                    },
                )
            }
            (None, Some(y)) => {
                stats.added += 1;
                ("added", y.kind)
            }
            (Some(x), None) => {
                stats.deleted += 1;
                ("deleted", x.kind)
            }
            _ => unreachable!(),
        };
        let diff_path = if status == "unchanged" {
            None
        } else {
            let relative = Path::new("diffs").join(format!("{path}.diff"));
            let relative = relative.to_string_lossy().to_string();
            let destination = out.join(&relative);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(destination, make_diff(old_path, new_path, a, b, context)?)?;
            Some(relative)
        };
        let old_content = snapshot_text(out, a, status)?;
        let new_content = snapshot_text(out, b, status)?;
        entries.push(ManifestEntry {
            path: path.to_string(),
            old_path: old_path.map(str::to_owned),
            new_path: new_path.map(str::to_owned),
            kind: kind.to_string(),
            status: status.to_string(),
            renamed,
            diff: diff_path,
            old_sha256: a.map(|x| x.digest.clone()),
            new_sha256: b.map(|x| x.digest.clone()),
            old_size: a.map(|x| x.size),
            new_size: b.map(|x| x.size),
            old_content,
            new_content,
        });
    }
    let manifest = Manifest {
        schema_version: 3,
        old: ArtifactInfo {
            name: old.name.clone(),
            sha256: old.digest.clone(),
        },
        new: ArtifactInfo {
            name: new.name.clone(),
            sha256: new.digest.clone(),
        },
        stats: stats.clone(),
        entries,
    };
    Ok((manifest, render_summary(old, new, &stats)))
}

struct EntryPair<'a> {
    old_path: Option<&'a str>,
    new_path: Option<&'a str>,
}

fn pair_entries<'a>(old: &'a Artifact, new: &'a Artifact) -> Vec<EntryPair<'a>> {
    let mut pairs = Vec::new();
    let mut unmatched_old: BTreeSet<&str> = old.entries.keys().map(String::as_str).collect();
    let mut unmatched_new: BTreeSet<&str> = new.entries.keys().map(String::as_str).collect();

    for path in old
        .entries
        .keys()
        .filter(|path| new.entries.contains_key(*path))
    {
        unmatched_old.remove(path.as_str());
        unmatched_new.remove(path.as_str());
        pairs.push(EntryPair {
            old_path: Some(path),
            new_path: Some(path),
        });
    }

    pair_unique_by(
        old,
        new,
        &mut unmatched_old,
        &mut unmatched_new,
        &mut pairs,
        |_, entry| format!("{}\0{}", entry.kind, entry.digest),
    );
    pair_unique_by(
        old,
        new,
        &mut unmatched_old,
        &mut unmatched_new,
        &mut pairs,
        |path, entry| format!("{}\0{}", entry.kind, versionless_path(path)),
    );

    pairs.extend(unmatched_old.into_iter().map(|path| EntryPair {
        old_path: Some(path),
        new_path: None,
    }));
    pairs.extend(unmatched_new.into_iter().map(|path| EntryPair {
        old_path: None,
        new_path: Some(path),
    }));
    pairs.sort_by(|left, right| {
        left.new_path
            .or(left.old_path)
            .cmp(&right.new_path.or(right.old_path))
            .then_with(|| left.old_path.cmp(&right.old_path))
    });
    pairs
}

fn pair_unique_by<'a>(
    old: &'a Artifact,
    new: &'a Artifact,
    unmatched_old: &mut BTreeSet<&'a str>,
    unmatched_new: &mut BTreeSet<&'a str>,
    pairs: &mut Vec<EntryPair<'a>>,
    key: impl Fn(&str, &Entry) -> String,
) {
    let mut old_groups = std::collections::BTreeMap::<String, Vec<&str>>::new();
    let mut new_groups = std::collections::BTreeMap::<String, Vec<&str>>::new();
    for path in unmatched_old.iter().copied() {
        old_groups
            .entry(key(path, &old.entries[path]))
            .or_default()
            .push(path);
    }
    for path in unmatched_new.iter().copied() {
        new_groups
            .entry(key(path, &new.entries[path]))
            .or_default()
            .push(path);
    }
    let matches: Vec<(&str, &str)> = old_groups
        .iter()
        .filter_map(|(key, old_paths)| {
            let new_paths = new_groups.get(key)?;
            (old_paths.len() == 1 && new_paths.len() == 1).then_some((old_paths[0], new_paths[0]))
        })
        .collect();
    for (old_path, new_path) in matches {
        unmatched_old.remove(old_path);
        unmatched_new.remove(new_path);
        pairs.push(EntryPair {
            old_path: Some(old_path),
            new_path: Some(new_path),
        });
    }
}

fn versionless_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut output = String::with_capacity(path.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            let first_digits = index - start;
            let mut components = 1;
            while index + 1 < bytes.len()
                && bytes[index] == b'.'
                && bytes[index + 1].is_ascii_digit()
            {
                components += 1;
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
            }
            if components >= 2 || first_digits >= 4 {
                output.push_str("{version}");
            } else {
                output.push_str(&path[start..index]);
            }
        } else {
            let character = path[index..].chars().next().expect("valid UTF-8 path");
            output.push(character);
            index += character.len_utf8();
        }
    }
    output
}

fn snapshot_text(out: &Path, entry: Option<&Entry>, status: &str) -> Result<Option<String>> {
    let Some(entry) = entry.filter(|entry| entry.kind == "text" && status != "unchanged") else {
        return Ok(None);
    };
    let relative = Path::new("blobs").join(&entry.digest);
    let destination = out.join(&relative);
    if !destination.exists() {
        std::fs::create_dir_all(out.join("blobs"))?;
        entry.content.copy_to(&destination)?;
    }
    Ok(Some(relative.to_string_lossy().into_owned()))
}

fn make_diff(
    old_path: Option<&str>,
    new_path: Option<&str>,
    old_entry: Option<&Entry>,
    new_entry: Option<&Entry>,
    context: usize,
) -> Result<String> {
    if let (Some(old), Some(new), Some(old_entry), Some(new_entry)) =
        (old_path, new_path, old_entry, new_entry)
    {
        if old != new && old_entry.digest == new_entry.digest {
            return Ok(format!(
                "similarity index 100%\nrename from {old}\nrename to {new}\n"
            ));
        }
    }
    let path = new_path.or(old_path).unwrap_or("entry");
    if old_entry.is_some_and(|entry| entry.kind != "text")
        || new_entry.is_some_and(|entry| entry.kind != "text")
    {
        return Ok(format!("Binary files {path} differ\n"));
    }
    let old = old_entry
        .map(|entry| entry.content.read())
        .transpose()?
        .unwrap_or_default();
    let new = new_entry
        .map(|entry| entry.content.read())
        .transpose()?
        .unwrap_or_default();
    let old_name = old_entry.map_or_else(
        || "/dev/null".to_string(),
        |_| format!("a/{}", old_path.unwrap_or(path)),
    );
    let new_name = new_entry.map_or_else(
        || "/dev/null".to_string(),
        |_| format!("b/{}", new_path.unwrap_or(path)),
    );
    Ok(String::from_utf8_lossy(&unified_diff::diff(
        &old, &old_name, &new, &new_name, context,
    ))
    .into_owned())
}

pub(crate) fn render_summary(old: &Artifact, new: &Artifact, stats: &Stats) -> String {
    format!(
        "{} -> {}\n\n{} unchanged\n{} modified\n{} renamed\n{} added\n{} deleted\n",
        old.name,
        new.name,
        stats.unchanged,
        stats.modified,
        stats.renamed,
        stats.added,
        stats.deleted
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Content;

    fn artifact(temp: &Path, name: &str, files: &[(&str, &str)]) -> Artifact {
        let mut artifact = Artifact {
            name: name.to_owned(),
            digest: name.to_owned(),
            ..Default::default()
        };
        for (index, (path, content)) in files.iter().enumerate() {
            let stored = temp.join(format!("{name}-{index}"));
            std::fs::write(&stored, content).unwrap();
            artifact.entries.insert(
                (*path).to_owned(),
                Entry {
                    path: (*path).to_owned(),
                    content: Content::File(stored),
                    digest: crate::scan::sha(content.as_bytes()),
                    size: content.len() as u64,
                    kind: "text",
                },
            );
        }
        artifact
    }

    #[test]
    fn removes_semantic_versions_and_long_build_numbers() {
        assert_eq!(
            versionless_path("lib/web-api-26.0.3.76224.jar"),
            "lib/web-api-{version}.jar"
        );
        assert_eq!(versionless_path("file2.txt"), "file2.txt");
    }

    #[test]
    fn pairs_unique_versioned_paths_and_records_sizes() {
        let temp = tempfile::tempdir().unwrap();
        let old = artifact(
            temp.path(),
            "old",
            &[("lib/web-api-26.0.3.76224.txt", "before\n")],
        );
        let new = artifact(
            temp.path(),
            "new",
            &[("lib/web-api-26.0.4.76507.txt", "after content\n")],
        );
        let (manifest, _) = compare(&old, &new, temp.path(), 3).unwrap();
        assert_eq!(manifest.stats.modified, 1);
        assert_eq!(manifest.stats.renamed, 1);
        assert_eq!(manifest.entries.len(), 1);
        let entry = &manifest.entries[0];
        assert!(entry.renamed);
        assert_eq!(
            entry.old_path.as_deref(),
            Some("lib/web-api-26.0.3.76224.txt")
        );
        assert_eq!(entry.new_size, Some(14));
    }

    #[test]
    fn does_not_guess_ambiguous_versioned_paths() {
        let temp = tempfile::tempdir().unwrap();
        let old = artifact(
            temp.path(),
            "old",
            &[("api-1.0.txt", "a"), ("api-2.0.txt", "b")],
        );
        let new = artifact(
            temp.path(),
            "new",
            &[("api-3.0.txt", "c"), ("api-4.0.txt", "d")],
        );
        let pairs = pair_entries(&old, &new);
        assert_eq!(
            pairs
                .iter()
                .filter(|pair| pair.old_path.is_some() && pair.new_path.is_some())
                .count(),
            0
        );
    }
}
