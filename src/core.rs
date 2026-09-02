use crate::cli::{Args, Color, JvmMode, NativeMode};
use crate::diff::{compare, render_summary};
use crate::model::*;
use crate::scan::{
    collect_dir, collect_path, sha, sha_file, strip_common_top_level, ArchiveLimits, ContentStore,
};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{IsTerminal, Read},
    path::{Component, Path, PathBuf},
    process::Command,
};
use zip::{write::SimpleFileOptions, ZipWriter};

struct LoadOptions<'a> {
    limits: ArchiveLimits,
    strip_top_level: bool,
    jadx: JadxOptions<'a>,
    workspace_dir: Option<&'a Path>,
}

struct JadxOptions<'a> {
    max_file: u64,
    mode: &'a JvmMode,
    path: &'a Path,
    cache_dir: Option<&'a Path>,
    no_cache: bool,
}

pub fn run(args: Args) -> Result<bool> {
    crate::progress::set_enabled(!args.quiet);
    let old_path = args.old_input();
    let new_path = args.new_input();
    if args.max_depth == 0 {
        bail!("--max-depth must be at least 1");
    }
    crate::progress::info(format!(
        "comparing {} with {}",
        old_path.display(),
        new_path.display()
    ));
    validate_output_path(&args.output, old_path, new_path)?;
    if let Some(workspace) = &args.workspace_dir {
        validate_workspace_path(workspace, old_path, new_path)?;
    }
    if matches!(args.native, NativeMode::Ida) {
        let ida = args
            .ida_path
            .as_deref()
            .context("--ida-path is required with --native ida")?;
        let script = args
            .diaphora_script
            .as_deref()
            .context("--diaphora-script is required with --native ida")?;
        let diaphora = args
            .diaphora_path
            .as_deref()
            .context("--diaphora-path is required with --native ida")?;
        if !ida.is_file() {
            bail!("IDA executable not found: {}", ida.display());
        }
        if !script.is_file() {
            bail!("Diaphora adapter script not found: {}", script.display());
        }
        if !diaphora.join("diaphora.py").is_file() || !diaphora.join("diaphora_ida.py").is_file() {
            bail!(
                "Diaphora directory must contain diaphora.py and diaphora_ida.py: {}",
                diaphora.display()
            );
        }
    }
    let native_config = args
        .ida_path
        .as_deref()
        .zip(args.diaphora_script.as_deref())
        .zip(args.diaphora_path.as_deref());
    if is_native_file(old_path)? && is_native_file(new_path)? {
        if let Some(((ida, script), diaphora)) =
            native_config.filter(|_| !matches!(args.native, NativeMode::Raw | NativeMode::Off))
        {
            return run_native_comparison(&args, ida, script, diaphora);
        }
        if matches!(args.native, NativeMode::Ida) {
            bail!("native analysis was requested but IDA configuration is incomplete");
        }
    }
    let jadx_path = if matches!(args.jvm, JvmMode::Raw | JvmMode::Off) {
        args.jadx_path.clone()
    } else {
        discover_jadx(&args.jadx_path)
    };
    if jadx_path != args.jadx_path && !matches!(args.jvm, JvmMode::Raw | JvmMode::Off) {
        crate::progress::info(format!("discovered JADX at {}", jadx_path.display()));
    }
    let load_options = LoadOptions {
        limits: ArchiveLimits {
            max_file: args.max_file_size,
            max_expanded: args.max_expanded_size,
            max_depth: args.max_depth,
        },
        strip_top_level: args.strip_top_level,
        jadx: JadxOptions {
            max_file: args.max_file_size,
            mode: &args.jvm,
            path: &jadx_path,
            cache_dir: args.cache_dir.as_deref(),
            no_cache: args.no_cache,
        },
        workspace_dir: args.workspace_dir.as_deref(),
    };
    let input_digests = if old_path.is_file() && new_path.is_file() {
        crate::progress::info("hashing file inputs before analysis");
        Some(hash_file_pair(old_path, new_path)?)
    } else {
        None
    };
    let reuse_old_scan = input_digests
        .as_ref()
        .is_some_and(|(old_digest, new_digest)| old_digest == new_digest)
        && (crate::scan::is_archive_file(old_path)?
            || old_path.file_name() == new_path.file_name());
    let parallel_tar_scan = !reuse_old_scan
        && args.max_depth == 1
        && std::thread::available_parallelism().is_ok_and(|parallelism| parallelism.get() > 1)
        && crate::scan::is_uncompressed_tar(old_path)?
        && crate::scan::is_uncompressed_tar(new_path)?;
    let (old, new) = if parallel_tar_scan {
        crate::progress::info("scanning both uncompressed TAR inputs concurrently");
        std::thread::scope(|scope| {
            let old_load = scope.spawn(|| {
                load_artifact_logged(
                    "old",
                    old_path,
                    &load_options,
                    input_digests.as_ref().map(|(digest, _)| digest.as_str()),
                )
            });
            let new = load_artifact_logged(
                "new",
                new_path,
                &load_options,
                input_digests.as_ref().map(|(_, digest)| digest.as_str()),
            )?;
            let old = old_load
                .join()
                .map_err(|_| anyhow::anyhow!("old-input scanning thread panicked"))??;
            Ok::<_, anyhow::Error>((old, new))
        })?
    } else {
        let old = load_artifact_logged(
            "old",
            old_path,
            &load_options,
            input_digests.as_ref().map(|(digest, _)| digest.as_str()),
        )?;
        let new = if reuse_old_scan {
            crate::progress::info("file inputs are identical; reusing the old content scan");
            Artifact {
                name: new_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                digest: old.digest.clone(),
                entries: old.entries.clone(),
                _workspace: None,
            }
        } else {
            load_artifact_logged(
                "new",
                new_path,
                &load_options,
                input_digests.as_ref().map(|(_, digest)| digest.as_str()),
            )?
        };
        (old, new)
    };
    crate::progress::info("generating comparison output");
    let output = crate::output::OutputTransaction::new(&args.output)?;
    let (manifest, summary) = compare(&old, &new, output.path(), args.context)?;
    crate::output::write_results(output.path(), &manifest, &summary)?;
    output.commit()?;
    print_summary(&render_summary(&old, &new, &manifest.stats), args.color);
    Ok(manifest.stats.added
        + manifest.stats.deleted
        + manifest.stats.modified
        + manifest.stats.renamed
        > 0)
}

/// Builds a persisted source-level comparison for a JAR pair retained by the TUI.
pub(crate) fn run_jadx_diff(
    old_blob: &Path,
    new_blob: &Path,
    old_name: &str,
    new_name: &str,
    destination: &Path,
    configured_jadx: &Path,
) -> Result<()> {
    const MAX_FILE: u64 = 256 * 1024 * 1024;
    const MAX_EXPANDED: u64 = 2 * 1024 * 1024 * 1024;

    let inputs = create_workspace(None, "on-demand-jadx")?;
    let old_input = inputs.path().join("old.jar");
    let new_input = inputs.path().join("new.jar");
    link_or_copy(old_blob, &old_input)?;
    link_or_copy(new_blob, &new_input)?;

    let jadx = discover_jadx(configured_jadx);
    let mode = JvmMode::Jadx;
    let options = LoadOptions {
        limits: ArchiveLimits {
            max_file: MAX_FILE,
            max_expanded: MAX_EXPANDED,
            max_depth: 1,
        },
        strip_top_level: false,
        jadx: JadxOptions {
            max_file: MAX_FILE,
            mode: &mode,
            path: &jadx,
            cache_dir: None,
            no_cache: true,
        },
        workspace_dir: None,
    };

    let (old_digest, new_digest) = hash_file_pair(&old_input, &new_input)?;
    let mut old = load_artifact(&old_input, &options, Some(&old_digest))?;
    old.name = old_name.to_owned();
    let mut new = if old_digest == new_digest {
        Artifact {
            name: new_name.to_owned(),
            digest: old.digest.clone(),
            entries: old.entries.clone(),
            _workspace: None,
        }
    } else {
        load_artifact(&new_input, &options, Some(&new_digest))?
    };
    new.name = new_name.to_owned();

    let output = crate::output::OutputTransaction::new(destination)?;
    let (manifest, summary) = compare(&old, &new, output.path(), 3)?;
    crate::output::write_results(output.path(), &manifest, &summary)?;
    output.commit()
}

fn link_or_copy(source: &Path, destination: &Path) -> Result<()> {
    std::fs::hard_link(source, destination)
        .or_else(|_| std::fs::copy(source, destination).map(|_| ()))
        .with_context(|| {
            format!(
                "materializing retained JAR {} as {}",
                source.display(),
                destination.display()
            )
        })
}

fn discover_jadx(configured: &Path) -> PathBuf {
    if configured.is_file() || configured.components().count() > 1 {
        return configured.to_path_buf();
    }
    if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(configured))
            .find(|candidate| candidate.is_file())
    }) {
        return path;
    }
    if configured != Path::new("jadx") {
        return configured.to_path_buf();
    }

    let mut candidates = Vec::new();
    if let Some(root) = std::env::var_os("JADX_HOME") {
        candidates.push(PathBuf::from(root).join("bin/jadx"));
    }
    candidates.extend([
        PathBuf::from("/opt/jadx/bin/jadx"),
        PathBuf::from("/usr/local/bin/jadx"),
    ]);
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("tools/bin/jadx"));
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| configured.to_path_buf())
}

fn load_artifact_logged(
    label: &str,
    path: &Path,
    options: &LoadOptions<'_>,
    digest_hint: Option<&str>,
) -> Result<Artifact> {
    crate::progress::info(format!("loading {label} input: {}", path.display()));
    let artifact = load_artifact(path, options, digest_hint)
        .with_context(|| format!("reading {}", path.display()))?;
    crate::progress::info(format!(
        "{label} input ready: {} logical entries",
        artifact.entries.len()
    ));
    Ok(artifact)
}

fn hash_file_pair(old: &Path, new: &Path) -> Result<(String, String)> {
    if std::thread::available_parallelism().is_ok_and(|parallelism| parallelism.get() > 1) {
        return std::thread::scope(|scope| {
            let old_hash = scope.spawn(|| sha_file(old));
            let new_hash = sha_file(new)?;
            let old_hash = old_hash
                .join()
                .map_err(|_| anyhow::anyhow!("old-input hashing thread panicked"))??;
            Ok((old_hash, new_hash))
        });
    }
    Ok((sha_file(old)?, sha_file(new)?))
}

fn run_native_comparison(args: &Args, ida: &Path, script: &Path, diaphora: &Path) -> Result<bool> {
    run_native_comparison_named(args, ida, script, diaphora, None, true)
}

fn run_native_comparison_named(
    args: &Args,
    ida: &Path,
    script: &Path,
    diaphora: &Path,
    names: Option<(&str, &str)>,
    emit_summary: bool,
) -> Result<bool> {
    crate::progress::info("native inputs detected; starting IDA/Diaphora analysis");
    let old_digest = sha_file(args.old_input())?;
    let new_digest = sha_file(args.new_input())?;
    let mut old = Artifact {
        name: names.map(|(old, _)| old.to_owned()).unwrap_or_else(|| {
            args.old_input()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }),
        digest: old_digest.clone(),
        ..Default::default()
    };
    let mut new = Artifact {
        name: names.map(|(_, new)| new.to_owned()).unwrap_or_else(|| {
            args.new_input()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }),
        digest: new_digest.clone(),
        ..Default::default()
    };
    if old_digest == new_digest {
        crate::progress::info("native inputs are identical; skipping IDA/Diaphora");
        let output = crate::output::OutputTransaction::new(&args.output)?;
        let (manifest, summary) = compare(&old, &new, output.path(), args.context)?;
        crate::output::write_results(output.path(), &manifest, &summary)?;
        std::fs::write(
            output.path().join("native-functions.json"),
            serde_json::to_vec_pretty(&crate::native::NativeResponse::empty())?,
        )?;
        output.commit()?;
        if emit_summary {
            print_summary(&summary, args.color);
        }
        return Ok(false);
    }

    let workspace = create_workspace(args.workspace_dir.as_deref(), "native")?;
    let response_path = workspace.path().join("response.json");
    let response = crate::native::run_adapter(
        ida,
        &args.python_path,
        script,
        diaphora,
        args.old_input(),
        args.new_input(),
        &response_path,
        workspace.path(),
        args.cache_dir.as_deref(),
        args.no_cache,
        &old_digest,
        &new_digest,
    )?;
    let native_changed = response
        .functions
        .iter()
        .any(|function| !matches!(function.status, crate::native::FunctionStatus::Unchanged));
    let store = ContentStore::new(workspace.path())?;
    let mut function_paths = BTreeSet::new();
    for function in &response.functions {
        let id = sanitize_component(&function.stable_id);
        let path = format!("functions/{id}.c");
        if !function_paths.insert(path.clone()) {
            bail!("native function IDs collide after path sanitization: {id}");
        }
        if let Some(source) = &function.old_pseudocode {
            let entry = store.stage_bytes(
                path.clone(),
                normalize_pseudocode(source).as_bytes(),
                args.max_file_size,
            )?;
            old.entries.insert(path.clone(), entry);
        }
        if let Some(source) = &function.new_pseudocode {
            let entry = store.stage_bytes(
                path.clone(),
                normalize_pseudocode(source).as_bytes(),
                args.max_file_size,
            )?;
            new.entries.insert(path, entry);
        }
    }
    let output = crate::output::OutputTransaction::new(&args.output)?;
    let (manifest, summary) = compare(&old, &new, output.path(), args.context)?;
    crate::output::write_results(output.path(), &manifest, &summary)?;
    std::fs::write(
        output.path().join("native-functions.json"),
        serde_json::to_vec_pretty(&response)?,
    )?;
    output.commit()?;
    if emit_summary {
        print_summary(&summary, args.color);
    }
    Ok(native_changed
        || manifest.stats.added
            + manifest.stats.deleted
            + manifest.stats.modified
            + manifest.stats.renamed
            > 0)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_native_diff(
    old_blob: &Path,
    new_blob: &Path,
    old_name: &str,
    new_name: &str,
    destination: &Path,
    ida: &Path,
    python: &Path,
    script: &Path,
    diaphora: &Path,
    cache_dir: Option<&Path>,
    no_cache: bool,
) -> Result<()> {
    if !ida.is_file() {
        bail!("IDA executable not found: {}", ida.display());
    }
    if !script.is_file() {
        bail!("Diaphora adapter script not found: {}", script.display());
    }
    if !diaphora.join("diaphora.py").is_file() || !diaphora.join("diaphora_ida.py").is_file() {
        bail!(
            "Diaphora directory must contain diaphora.py and diaphora_ida.py: {}",
            diaphora.display()
        );
    }
    let args = Args {
        old: Some(old_blob.to_path_buf()),
        new: Some(new_blob.to_path_buf()),
        view: None,
        tui: false,
        output: destination.to_path_buf(),
        color: Color::Never,
        context: 3,
        max_file_size: 256 * 1024 * 1024,
        max_expanded_size: 2 * 1024 * 1024 * 1024,
        max_depth: 1,
        jvm: JvmMode::Raw,
        jadx_path: PathBuf::from("jadx"),
        cache_dir: cache_dir.map(Path::to_path_buf),
        workspace_dir: None,
        no_cache,
        native: NativeMode::Ida,
        ida_path: Some(ida.to_path_buf()),
        diaphora_script: Some(script.to_path_buf()),
        diaphora_path: Some(diaphora.to_path_buf()),
        python_path: python.to_path_buf(),
        strip_top_level: false,
        quiet: true,
    };
    run_native_comparison_named(
        &args,
        ida,
        script,
        diaphora,
        Some((old_name, new_name)),
        false,
    )?;
    Ok(())
}

fn is_native_file(path: &Path) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let mut magic = [0_u8; 4];
    let read = File::open(path)?.read(&mut magic)?;
    Ok(crate::classify::is_native_magic(&magic[..read]))
}

fn sanitize_component(value: &str) -> String {
    let mut sanitized: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.len() > 120 {
        sanitized.truncate(100);
        sanitized.push('_');
        sanitized.push_str(&sha(value.as_bytes())[..16]);
    }
    if sanitized.is_empty() {
        "function".to_string()
    } else {
        sanitized
    }
}

fn normalize_pseudocode(source: &str) -> String {
    source.replace("\r\n", "\n").replace('\r', "\n")
}

fn print_summary(summary: &str, color: Color) {
    let enabled = matches!(color, Color::Always)
        || matches!(color, Color::Auto) && std::io::stdout().is_terminal();
    if enabled {
        println!("\x1b[1m{summary}\x1b[0m");
    } else {
        print!("{summary}");
    }
}

fn validate_output_path(output: &Path, old: &Path, new: &Path) -> Result<()> {
    let base = std::env::current_dir()?;
    let absolute_output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        base.join(output)
    };
    let output = normalize_path(&absolute_output);
    let old = std::fs::canonicalize(old)?;
    let new = std::fs::canonicalize(new)?;
    if old.starts_with(&output)
        || new.starts_with(&output)
        || output.starts_with(&old)
        || output.starts_with(&new)
    {
        bail!("output directory cannot overlap either input");
    }
    Ok(())
}

fn validate_workspace_path(workspace: &Path, old: &Path, new: &Path) -> Result<()> {
    let base = std::env::current_dir()?;
    let absolute_workspace = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        base.join(workspace)
    };
    let workspace = normalize_path(&absolute_workspace);
    let old = std::fs::canonicalize(old)?;
    let new = std::fs::canonicalize(new)?;
    if workspace.starts_with(&old) || workspace.starts_with(&new) {
        bail!("workspace directory cannot be inside either input");
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn load_artifact(
    path: &Path,
    options: &LoadOptions<'_>,
    digest_hint: Option<&str>,
) -> Result<Artifact> {
    let workspace = create_workspace(options.workspace_dir, "artifact")?;
    let store = ContentStore::new(workspace.path())?;
    let mut root = Artifact {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        _workspace: Some(workspace),
        ..Default::default()
    };
    if path.is_dir() {
        let mut expanded = 0;
        collect_dir(
            path,
            path,
            &mut root.entries,
            &store,
            options.limits,
            &mut expanded,
        )?;
        decompile_nested_jars(&mut root.entries, &store, &options.jadx)?;
        let mut h = Sha256::new();
        for entry in root.entries.values() {
            h.update(entry.path.as_bytes());
            h.update(entry.digest.as_bytes());
        }
        root.digest = format!("{:x}", h.finalize());
    } else {
        root.digest = digest_hint
            .map(str::to_owned)
            .map_or_else(|| sha_file(path), Ok)?;
        let mut expanded = 0;
        collect_path(
            path,
            &mut root.entries,
            &store,
            options.limits,
            &mut expanded,
        )?;
        decompile_nested_jars(&mut root.entries, &store, &options.jadx)?;
        if is_jar_path(path) {
            maybe_decompile_jar(&mut root.entries, &store, path, &root.digest, &options.jadx)?;
        }
    }
    if options.strip_top_level {
        strip_common_top_level(&mut root.entries);
    }
    Ok(root)
}

fn create_workspace(parent: Option<&Path>, label: &str) -> Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    let prefix = format!("artifact-diff-{label}-");
    builder.prefix(&prefix);
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)?;
        builder
            .tempdir_in(parent)
            .with_context(|| format!("creating workspace in {}", parent.display()))
    } else {
        builder.tempdir().context("creating temporary workspace")
    }
}

fn is_jar_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("jar"))
}

fn decompile_nested_jars(
    entries: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    options: &JadxOptions<'_>,
) -> Result<()> {
    if matches!(options.mode, JvmMode::Off | JvmMode::Raw) {
        return Ok(());
    }

    let mut prefixes: Vec<String> = entries
        .keys()
        .flat_map(|path| nested_jar_prefixes(path))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    prefixes.sort_by_key(|prefix| std::cmp::Reverse(prefix.matches("!/").count()));

    for prefix in prefixes {
        let members: Vec<(String, Content)> = entries
            .iter()
            .filter_map(|(path, entry)| {
                let rest = path.strip_prefix(&format!("{prefix}!/"))?;
                if !rest.contains("!/") {
                    Some((rest.to_string(), entry.content.clone()))
                } else {
                    None
                }
            })
            .collect();
        if !members.iter().any(|(path, _)| path.ends_with(".class")) {
            continue;
        }

        let workspace = tempfile::tempdir().context("creating nested JADX workspace")?;
        let input = workspace.path().join("nested.jar");
        {
            let mut writer = ZipWriter::new(File::create(&input)?);
            for (path, content) in &members {
                writer.start_file(path, SimpleFileOptions::default())?;
                let mut source = content.open()?;
                std::io::copy(&mut source, &mut writer)?;
            }
            writer.finish()?;
        }
        if std::fs::metadata(&input)?.len() > options.max_file {
            bail!("nested JAR {} exceeds max-file-size", prefix);
        }
        let mut generated = BTreeMap::new();
        maybe_decompile_jar(&mut generated, store, &input, &sha_file(&input)?, options)?;
        if generated.is_empty() {
            continue;
        }

        entries.retain(|path, _| {
            path.strip_prefix(&format!("{prefix}!/"))
                .is_none_or(|rest| rest.contains("!/"))
        });
        for (path, entry) in generated {
            let logical = format!("{prefix}!/{path}");
            entries.insert(
                logical.clone(),
                Entry {
                    path: logical,
                    ..entry
                },
            );
        }
    }
    Ok(())
}

fn nested_jar_prefixes(path: &str) -> Vec<String> {
    path.match_indices("!/")
        .filter_map(|(index, _)| {
            let prefix = &path[..index];
            is_jar_path(Path::new(prefix)).then(|| prefix.to_string())
        })
        .collect()
}

fn maybe_decompile_jar(
    entries: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    jar: &Path,
    digest: &str,
    options: &JadxOptions<'_>,
) -> Result<()> {
    if matches!(options.mode, JvmMode::Off | JvmMode::Raw) {
        return Ok(());
    }

    let cache = options
        .cache_dir
        .map(|root| root.join("jadx").join(jadx_cache_key(options.path, digest)));
    let source_dir = if !options.no_cache {
        cache.as_deref().filter(|path| path.is_dir())
    } else {
        None
    };

    let temporary;
    let generated = if let Some(cached) = source_dir {
        crate::progress::info(format!("using cached JADX output for {}", jar.display()));
        cached.to_path_buf()
    } else {
        temporary = tempfile::tempdir().context("creating JADX workspace")?;
        let output = temporary.path().join("sources");
        crate::progress::info(format!("running JADX for {}", jar.display()));
        let result = crate::process::run(
            Command::new(options.path).arg("-d").arg(&output).arg(jar),
            "JADX",
        );
        if let Err(error) = result {
            if matches!(options.mode, JvmMode::Jadx) {
                return Err(error).context("decompiling with JADX");
            }
            crate::progress::info(format!(
                "JADX unavailable or failed; keeping raw classes: {error}"
            ));
            return Ok(());
        }
        if let Some(cache_path) = &cache {
            copy_directory(&output, cache_path)?;
        }
        output
    };

    let mut generated_entries = BTreeMap::new();
    collect_generated_sources(&generated, &mut generated_entries, store, options.max_file)?;
    if generated_entries.is_empty() && matches!(options.mode, JvmMode::Jadx) {
        bail!("JADX produced no Java sources for {}", jar.display());
    }
    entries.retain(|path, _| !path.ends_with(".class"));
    entries.extend(generated_entries);
    Ok(())
}

fn jadx_cache_key(jadx_path: &Path, digest: &str) -> String {
    let version = Command::new(jadx_path)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    sha(format!("protocol=2\nversion={version}\ndigest={digest}\n").as_bytes())
}

fn collect_generated_sources(
    root: &Path,
    entries: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    max_file: u64,
) -> Result<()> {
    collect_generated_sources_from(root, root, entries, store, max_file)
}

fn collect_generated_sources_from(
    root: &Path,
    current: &Path,
    entries: &mut BTreeMap<String, Entry>,
    store: &ContentStore,
    max_file: u64,
) -> Result<()> {
    if !current.is_dir() {
        return Ok(());
    }
    for item in std::fs::read_dir(current)? {
        let path = item?.path();
        if path.is_dir() {
            collect_generated_sources_from(root, &path, entries, store, max_file)?;
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("java") {
            continue;
        }
        if std::fs::metadata(&path)?.len() > max_file {
            bail!("generated source exceeds max-file-size: {}", path.display());
        }
        let relative = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        let source = normalize_java(&std::fs::read_to_string(&path)?);
        let entry = store.stage_bytes(relative.clone(), source.as_bytes(), max_file)?;
        entries.insert(relative, entry);
    }
    Ok(())
}

fn normalize_java(source: &str) -> String {
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .lines()
        .filter_map(|line| {
            let upper = line.to_ascii_uppercase();
            if upper.contains("JADX") || upper.contains("LOADED FROM:") {
                return None;
            }
            let line = line.trim_end();
            Some(
                if line.contains('(')
                    && line.contains(')')
                    && line.contains("final ")
                    && !line.trim_start().starts_with("//")
                {
                    line.replace("final ", "")
                } else {
                    line.to_string()
                },
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if normalized.ends_with('\n') { "\n" } else { "" }
}

fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for item in std::fs::read_dir(source)? {
        let item = item?;
        let from = item.path();
        let to = destination.join(item.file_name());
        if from.is_dir() {
            copy_directory(&from, &to)?;
        } else {
            std::fs::copy(from, to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_java_line_endings() {
        assert_eq!(
            normalize_java(
                "/* JADX INFO: loaded from: old.jar */\r\npublic void f(final String x)  \r\n"
            ),
            "public void f(String x)\n"
        );
    }

    #[test]
    fn generated_sources_preserve_package_paths() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("com/example");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("Main.java"), "class Main {}\n").unwrap();
        let mut entries = BTreeMap::new();
        let workspace = tempfile::tempdir().unwrap();
        let store = ContentStore::new(workspace.path()).unwrap();
        collect_generated_sources(temp.path(), &mut entries, &store, 1024).unwrap();
        assert!(entries.contains_key("com/example/Main.java"));
    }

    #[test]
    fn finds_every_nested_jar_prefix() {
        assert_eq!(
            nested_jar_prefixes("outer.jar!/lib/inner.jar!/a/A.class"),
            ["outer.jar", "outer.jar!/lib/inner.jar"]
        );
    }

    #[test]
    fn normalizes_parent_components() {
        assert_eq!(
            normalize_path(Path::new("/tmp/results/../input")),
            Path::new("/tmp/input")
        );
    }

    #[cfg(unix)]
    #[test]
    fn on_demand_native_diff_uses_supplied_toolchain() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old-blob");
        let new = temp.path().join("new-blob");
        std::fs::write(&old, b"\x7fELFold").unwrap();
        std::fs::write(&new, b"\x7fELFnew").unwrap();

        let ida = temp.path().join("fake-ida");
        std::fs::write(
            &ida,
            "#!/bin/sh\npython3 -c 'import json,os; r=json.load(open(os.environ[\"ARTIFACT_DIFF_REQUEST\"])); open(r[\"export_database\"],\"wb\").write(b\"sqlite\")'\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&ida).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&ida, permissions).unwrap();

        let adapter = temp.path().join("adapter.py");
        std::fs::write(
            &adapter,
            "import json,os\nr=json.load(open(os.environ['ARTIFACT_DIFF_REQUEST']))\nf={'stable_id':'main','old_address':1,'new_address':2,'old_name':'main','new_name':'main','status':'modified','similarity':0.9,'old_pseudocode':'int main() { return 1; }','new_pseudocode':'int main() { return 2; }'}\njson.dump({'protocol_version':1,'functions':[f]},open(r['output'],'w'))\n",
        )
        .unwrap();
        let diaphora = temp.path().join("diaphora");
        std::fs::create_dir(&diaphora).unwrap();
        std::fs::write(diaphora.join("diaphora.py"), "").unwrap();
        std::fs::write(diaphora.join("diaphora_ida.py"), "").unwrap();
        let output = temp.path().join("result");

        run_native_diff(
            &old,
            &new,
            "legacy-service",
            "replacement-service",
            &output,
            &ida,
            Path::new("python3"),
            &adapter,
            &diaphora,
            None,
            true,
        )
        .unwrap();

        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.old.name, "legacy-service");
        assert_eq!(manifest.new.name, "replacement-service");
        assert_eq!(manifest.stats.modified, 1);
        assert!(output.join("diffs/functions/main.c.diff").is_file());
    }
}
