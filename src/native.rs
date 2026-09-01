//! Native-analysis protocol shared by Rust and the IDA/Diaphora adapter.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path, process::Command, time::UNIX_EPOCH};

const PROTOCOL_VERSION: u32 = 1;
const MAX_RESPONSE_SIZE: u64 = 256 * 1024 * 1024;
const CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize, Serialize)]
struct CacheMetadata {
    schema_version: u32,
    key: String,
    export_size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct FunctionDiff {
    pub stable_id: String,
    pub old_address: Option<u64>,
    pub new_address: Option<u64>,
    pub old_name: Option<String>,
    pub new_name: Option<String>,
    pub status: FunctionStatus,
    pub similarity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_reason: Option<String>,
    pub old_pseudocode: Option<String>,
    pub new_pseudocode: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FunctionStatus {
    Added,
    Deleted,
    Modified,
    Unchanged,
    Unresolved,
}

#[derive(Debug, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum AdapterRequest<'a> {
    Export {
        protocol_version: u32,
        input: &'a Path,
        export_database: &'a Path,
        diaphora_path: &'a Path,
    },
    Compare {
        protocol_version: u32,
        old_database: &'a Path,
        new_database: &'a Path,
        results_database: &'a Path,
        output: &'a Path,
        diaphora_path: &'a Path,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeResponse {
    pub protocol_version: u32,
    pub functions: Vec<FunctionDiff>,
}

impl NativeResponse {
    pub(crate) fn empty() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            functions: Vec::new(),
        }
    }
}

/// Export both inputs under IDA, compare their databases through Diaphora, and
/// validate the adapter's versioned JSON response.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_adapter(
    ida: &Path,
    python: &Path,
    adapter: &Path,
    diaphora_path: &Path,
    old_input: &Path,
    new_input: &Path,
    output: &Path,
    workspace: &Path,
    cache_dir: Option<&Path>,
    no_cache: bool,
    old_digest: &str,
    new_digest: &str,
) -> Result<NativeResponse> {
    let old_database = workspace.join("old.sqlite");
    let new_database = workspace.join("new.sqlite");
    crate::progress::info(format!(
        "exporting old binary with IDA: {}",
        old_input.display()
    ));
    obtain_export(
        ida,
        adapter,
        diaphora_path,
        old_input,
        &old_database,
        &workspace.join("old.i64"),
        &workspace.join("old-export-request.json"),
        cache_dir,
        no_cache,
        old_digest,
    )?;
    crate::progress::info(format!(
        "exporting new binary with IDA: {}",
        new_input.display()
    ));
    obtain_export(
        ida,
        adapter,
        diaphora_path,
        new_input,
        &new_database,
        &workspace.join("new.i64"),
        &workspace.join("new-export-request.json"),
        cache_dir,
        no_cache,
        new_digest,
    )?;

    let results_database = workspace.join("results.diaphora");
    let compare_request = AdapterRequest::Compare {
        protocol_version: PROTOCOL_VERSION,
        old_database: &old_database,
        new_database: &new_database,
        results_database: &results_database,
        output,
        diaphora_path,
    };
    let request_path = workspace.join("compare-request.json");
    write_request(&request_path, &compare_request)?;
    crate::progress::info("matching exported functions with Diaphora");
    crate::process::run(
        Command::new(python)
            .arg(adapter)
            .env("ARTIFACT_DIFF_REQUEST", &request_path),
        "Diaphora comparison",
    )
    .with_context(|| format!("running Python adapter at {}", python.display()))?;

    let bytes = read_bounded(output, MAX_RESPONSE_SIZE)
        .with_context(|| format!("reading native response {}", output.display()))?;
    let response: NativeResponse = serde_json::from_slice(&bytes)?;
    validate_response(&response)?;
    crate::progress::info(format!(
        "Diaphora returned {} function records",
        response.functions.len()
    ));
    Ok(response)
}

#[allow(clippy::too_many_arguments)]
fn obtain_export(
    ida: &Path,
    adapter: &Path,
    diaphora_path: &Path,
    input: &Path,
    export_database_path: &Path,
    ida_database: &Path,
    request_path: &Path,
    cache_dir: Option<&Path>,
    no_cache: bool,
    digest: &str,
) -> Result<()> {
    let cache = if no_cache {
        None
    } else {
        cache_dir
            .map(|root| native_cache_path(root, ida, adapter, diaphora_path, digest))
            .transpose()?
    };
    if let Some(cache) = cache.as_deref().filter(|path| valid_cache_entry(path)) {
        crate::progress::info(format!("using cached IDA export for {}", input.display()));
        std::fs::copy(cache.join("export.sqlite"), export_database_path)?;
        return Ok(());
    }

    export_database(
        ida,
        adapter,
        diaphora_path,
        input,
        export_database_path,
        ida_database,
        request_path,
    )?;
    if let Some(cache) = cache {
        store_cache_entry(&cache, export_database_path)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn export_database(
    ida: &Path,
    adapter: &Path,
    diaphora_path: &Path,
    input: &Path,
    export_database: &Path,
    ida_database: &Path,
    request_path: &Path,
) -> Result<()> {
    let request = AdapterRequest::Export {
        protocol_version: PROTOCOL_VERSION,
        input,
        export_database,
        diaphora_path,
    };
    write_request(request_path, &request)?;
    crate::process::run(
        Command::new(ida)
            .arg("-A")
            .arg("-B")
            .arg(format!("-o{}", ida_database.display()))
            .arg(format!("-S{}", adapter.display()))
            .arg(input)
            .env("ARTIFACT_DIFF_REQUEST", request_path),
        "IDA export",
    )
    .with_context(|| format!("running IDA at {}", ida.display()))?;
    if !export_database.is_file() {
        bail!(
            "IDA export completed without producing {}",
            export_database.display()
        );
    }
    Ok(())
}

fn native_cache_path(
    root: &Path,
    ida: &Path,
    adapter: &Path,
    diaphora_path: &Path,
    digest: &str,
) -> Result<std::path::PathBuf> {
    let identity = format!(
        "cache={CACHE_SCHEMA_VERSION}\nprotocol={PROTOCOL_VERSION}\nbinary={digest}\nida={}\nadapter={}\ndiaphora={}\ndiaphora_ida={}\ndecompiler=1\n",
        executable_identity(ida)?,
        crate::scan::sha_file(adapter)?,
        crate::scan::sha_file(&diaphora_path.join("diaphora.py"))?,
        crate::scan::sha_file(&diaphora_path.join("diaphora_ida.py"))?,
    );
    Ok(root
        .join("native")
        .join(crate::scan::sha(identity.as_bytes())))
}

fn executable_identity(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(format!(
        "{}:{}:{modified}",
        std::fs::canonicalize(path)?.display(),
        metadata.len()
    ))
}

fn valid_cache_entry(path: &Path) -> bool {
    let database = path.join("export.sqlite");
    let metadata = std::fs::read(path.join("metadata.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CacheMetadata>(&bytes).ok());
    metadata.is_some_and(|metadata| {
        metadata.schema_version == CACHE_SCHEMA_VERSION
            && metadata.key == path.file_name().unwrap_or_default().to_string_lossy()
            && std::fs::metadata(database)
                .is_ok_and(|file| file.is_file() && file.len() == metadata.export_size)
    })
}

fn store_cache_entry(path: &Path, database: &Path) -> Result<()> {
    if valid_cache_entry(path) {
        return Ok(());
    }
    if path.exists() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("removing invalid native cache entry {}", path.display()))?;
    }
    let parent = path.parent().context("native cache path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let staged = tempfile::Builder::new()
        .prefix(".native-cache-")
        .tempdir_in(parent)?;
    let export_size = std::fs::copy(database, staged.path().join("export.sqlite"))?;
    let metadata = CacheMetadata {
        schema_version: CACHE_SCHEMA_VERSION,
        key: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        export_size,
    };
    std::fs::write(
        staged.path().join("metadata.json"),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    let staged = staged.keep();
    match std::fs::rename(&staged, path) {
        Ok(()) => {}
        Err(_) if path.exists() => std::fs::remove_dir_all(staged)?,
        Err(error) => return Err(error).context("committing native cache entry"),
    }
    Ok(())
}

fn write_request(path: &Path, request: &AdapterRequest<'_>) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(request)?)?;
    Ok(())
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("native response exceeds size limit");
    }
    Ok(bytes)
}

fn validate_response(response: &NativeResponse) -> Result<()> {
    if response.protocol_version != PROTOCOL_VERSION {
        bail!(
            "unsupported native adapter protocol version {}",
            response.protocol_version
        );
    }
    let mut ids = BTreeSet::new();
    for function in &response.functions {
        if function.stable_id.trim().is_empty()
            || function.stable_id.len() > 512
            || !ids.insert(&function.stable_id)
        {
            bail!("native adapter returned an empty or duplicate stable function ID");
        }
        if function
            .similarity
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            bail!("invalid similarity for function {}", function.stable_id);
        }
        let valid_sides = match function.status {
            FunctionStatus::Added => {
                function.old_pseudocode.is_none() && function.new_pseudocode.is_some()
            }
            FunctionStatus::Deleted => {
                function.old_pseudocode.is_some() && function.new_pseudocode.is_none()
            }
            FunctionStatus::Modified | FunctionStatus::Unchanged => {
                function.old_pseudocode.is_some() && function.new_pseudocode.is_some()
            }
            FunctionStatus::Unresolved => true,
        };
        if !valid_sides {
            bail!(
                "native adapter returned inconsistent status for {}",
                function.stable_id
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_status_serializes_stably() {
        assert_eq!(
            serde_json::to_string(&FunctionStatus::Unresolved).unwrap(),
            "\"unresolved\""
        );
    }

    #[cfg(unix)]
    #[test]
    fn adapter_protocol_round_trip() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
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
            "import json,os\nr=json.load(open(os.environ['ARTIFACT_DIFF_REQUEST']))\njson.dump({'protocol_version':1,'functions':[]},open(r['output'],'w'))\n",
        )
        .unwrap();
        let old = temp.path().join("old.bin");
        let new = temp.path().join("new.bin");
        let output = temp.path().join("response.json");
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();

        let response = run_adapter(
            &ida,
            Path::new("python3"),
            &adapter,
            temp.path(),
            &old,
            &new,
            &output,
            temp.path(),
            None,
            false,
            "old-digest",
            "new-digest",
        )
        .unwrap();
        assert_eq!(response.protocol_version, 1);
        assert!(response.functions.is_empty());
    }

    #[test]
    fn rejects_duplicate_stable_ids() {
        let function = FunctionDiff {
            stable_id: "same".into(),
            old_address: Some(1),
            new_address: Some(2),
            old_name: None,
            new_name: None,
            status: FunctionStatus::Unchanged,
            similarity: Some(1.0),
            match_category: Some("best".into()),
            match_reason: None,
            old_pseudocode: Some("x".into()),
            new_pseudocode: Some("x".into()),
        };
        let response = NativeResponse {
            protocol_version: 1,
            functions: vec![function.clone(), function],
        };
        assert!(validate_response(&response).is_err());
    }
}
