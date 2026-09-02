//! Stdio Model Context Protocol server for agent-driven patch analysis.

use crate::{
    cli::{Args, Color, JvmMode, NativeMode},
    model::Manifest,
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    path::{Component, Path, PathBuf},
};

const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_MANIFEST_SIZE: u64 = 64 * 1024 * 1024;
const DEFAULT_DIFF_SIZE: usize = 256 * 1024;
const MAX_DIFF_SIZE: usize = 2 * 1024 * 1024;
const MAX_CHANGE_COUNT: usize = 1_000;
const MAX_CONTEXT_LINES: usize = 10_000;
const MAX_ARCHIVE_DEPTH: usize = 32;

pub(crate) fn run(args: Args) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve(args, stdin.lock(), stdout.lock())
}

fn serve<R: BufRead, W: Write>(base: Args, reader: R, mut writer: W) -> Result<()> {
    let mut initialized = false;
    for line in reader.lines() {
        let line = line.context("reading MCP request from stdin")?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_message(
                    &mut writer,
                    rpc_error(Value::Null, -32700, error.to_string()),
                )?;
                continue;
            }
        };
        if let Some(response) = handle_request(&base, &request, &mut initialized) {
            write_message(&mut writer, response)?;
        }
    }
    Ok(())
}

fn handle_request(base: &Args, request: &Value, initialized: &mut bool) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
        return id.map(|id| rpc_error(id, -32600, "invalid JSON-RPC request"));
    }
    let method = method.expect("method was checked");
    let modern = is_modern_request(request);

    if id.is_none() {
        if method == "notifications/initialized" {
            *initialized = true;
        }
        return None;
    }
    let id = id.expect("request ID exists");
    match method {
        "initialize" => {
            *initialized = true;
            let requested = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(LEGACY_PROTOCOL_VERSION);
            Some(rpc_result(
                id,
                json!({
                    "protocolVersion": negotiate_version(requested),
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "diffplus",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "instructions": "Compare local software artifacts and inspect bounded patch diffs. Comparisons replace the explicitly selected output directory."
                }),
            ))
        }
        "server/discover" => Some(rpc_result(
            id,
            json!({
                "resultType": "complete",
                "supportedVersions": [MODERN_PROTOCOL_VERSION],
                "capabilities": { "tools": {} },
                "instructions": "Compare local software artifacts and inspect bounded patch diffs.",
            }),
        )),
        "ping" => Some(rpc_result(id, json!({}))),
        _ if !*initialized && !modern => {
            Some(rpc_error(id, -32002, "MCP server is not initialized"))
        }
        "tools/list" => Some(rpc_result(id, json!({ "tools": tool_definitions() }))),
        "tools/call" => Some(rpc_result(
            id.clone(),
            match call_tool(base, request.get("params")) {
                Ok(result) => result,
                Err(error) => return Some(rpc_error(id, -32602, error.to_string())),
            },
        )),
        _ => Some(rpc_error(id, -32601, format!("method not found: {method}"))),
    }
}

fn call_tool(base: &Args, params: Option<&Value>) -> Result<Value> {
    let name = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .context("tools/call requires a tool name")?;
    let arguments = params
        .and_then(|params| params.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let result = match name {
        "compare_artifacts" => parse_and_call(arguments, |input| compare_artifacts(base, input)),
        "list_changes" => parse_and_call(arguments, list_changes),
        "read_diff" => parse_and_call(arguments, read_diff),
        _ => bail!("unknown tool: {name}"),
    };
    Ok(tool_result(result))
}

fn parse_and_call<T, F>(arguments: Value, operation: F) -> Result<Value>
where
    T: for<'de> Deserialize<'de>,
    F: FnOnce(T) -> Result<Value>,
{
    let input = serde_json::from_value(arguments).context("invalid tool arguments")?;
    operation(input)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompareInput {
    old_path: PathBuf,
    new_path: PathBuf,
    output_path: PathBuf,
    context: Option<usize>,
    max_depth: Option<usize>,
    jvm: Option<JvmMode>,
    native: Option<NativeMode>,
    strip_top_level: Option<bool>,
    no_cache: Option<bool>,
}

fn compare_artifacts(base: &Args, input: CompareInput) -> Result<Value> {
    if input.context.is_some_and(|value| value > MAX_CONTEXT_LINES) {
        bail!("context must not exceed {MAX_CONTEXT_LINES}");
    }
    if input
        .max_depth
        .is_some_and(|value| value == 0 || value > MAX_ARCHIVE_DEPTH)
    {
        bail!("max_depth must be between 1 and {MAX_ARCHIVE_DEPTH}");
    }
    let mut args = base.clone();
    args.mcp = false;
    args.old = Some(input.old_path);
    args.new = Some(input.new_path);
    args.view = None;
    args.tui = false;
    args.output = input.output_path;
    args.color = Color::Never;
    if let Some(value) = input.context {
        args.context = value;
    }
    if let Some(value) = input.max_depth {
        args.max_depth = value;
    }
    if let Some(value) = input.jvm {
        args.jvm = value;
    }
    if let Some(value) = input.native {
        args.native = value;
    }
    if let Some(value) = input.strip_top_level {
        args.strip_top_level = value;
    }
    if let Some(value) = input.no_cache {
        args.no_cache = value;
    }

    let output = args.output.clone();
    let changed = crate::core::run_for_mcp(args)?;
    let manifest = load_manifest(&output)?;
    let output = std::fs::canonicalize(&output).unwrap_or(output);
    let change_count = manifest.stats.added
        + manifest.stats.deleted
        + manifest.stats.modified
        + manifest.stats.renamed;
    Ok(json!({
        "changed": changed,
        "change_count": change_count,
        "output_path": output,
        "old": manifest.old,
        "new": manifest.new,
        "stats": manifest.stats
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListChangesInput {
    result_path: PathBuf,
    status: Option<String>,
    path_prefix: Option<String>,
    #[serde(default)]
    include_unchanged: bool,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

fn list_changes(input: ListChangesInput) -> Result<Value> {
    let limit = input.limit.unwrap_or(100);
    if limit == 0 || limit > MAX_CHANGE_COUNT {
        bail!("limit must be between 1 and {MAX_CHANGE_COUNT}");
    }
    if let Some(status) = input.status.as_deref() {
        if !matches!(status, "added" | "deleted" | "modified" | "unchanged") {
            bail!("status must be added, deleted, modified, or unchanged");
        }
    }
    let manifest = load_manifest(&input.result_path)?;
    let matching: Vec<_> = manifest
        .entries
        .iter()
        .filter(|entry| input.include_unchanged || entry.status != "unchanged")
        .filter(|entry| {
            input
                .status
                .as_deref()
                .is_none_or(|status| entry.status == status)
        })
        .filter(|entry| {
            input
                .path_prefix
                .as_deref()
                .is_none_or(|prefix| entry.path.starts_with(prefix))
        })
        .collect();
    let total = matching.len();
    let entries: Vec<_> = matching
        .into_iter()
        .skip(input.offset)
        .take(limit)
        .collect();
    let returned = entries.len();
    let next_offset = (input.offset + returned < total).then_some(input.offset + returned);
    Ok(json!({
        "entries": entries,
        "offset": input.offset,
        "returned": returned,
        "total": total,
        "next_offset": next_offset
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadDiffInput {
    result_path: PathBuf,
    path: String,
    max_bytes: Option<usize>,
}

fn read_diff(input: ReadDiffInput) -> Result<Value> {
    let max_bytes = input.max_bytes.unwrap_or(DEFAULT_DIFF_SIZE);
    if max_bytes == 0 || max_bytes > MAX_DIFF_SIZE {
        bail!("max_bytes must be between 1 and {MAX_DIFF_SIZE}");
    }
    let manifest = load_manifest(&input.result_path)?;
    let entry = manifest
        .entries
        .iter()
        .find(|entry| {
            entry.path == input.path
                || entry.old_path.as_deref() == Some(input.path.as_str())
                || entry.new_path.as_deref() == Some(input.path.as_str())
        })
        .with_context(|| format!("change not found in manifest: {}", input.path))?;
    let relative = entry
        .diff
        .as_deref()
        .with_context(|| format!("change has no textual diff: {}", input.path))?;
    let path = safe_result_file(&input.result_path, relative)?;
    let total_bytes = std::fs::metadata(&path)?.len();
    let mut bytes = Vec::with_capacity(max_bytes.min(total_bytes as usize));
    std::fs::File::open(&path)?
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > max_bytes;
    bytes.truncate(max_bytes);
    Ok(json!({
        "path": entry.path,
        "diff": String::from_utf8_lossy(&bytes),
        "truncated": truncated,
        "total_bytes": total_bytes
    }))
}

fn load_manifest(result: &Path) -> Result<Manifest> {
    let path = result.join("manifest.json");
    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("reading result manifest metadata {}", path.display()))?;
    if !metadata.is_file() {
        bail!("result manifest is not a file: {}", path.display());
    }
    if metadata.len() > MAX_MANIFEST_SIZE {
        bail!("result manifest exceeds {MAX_MANIFEST_SIZE} bytes");
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading result manifest {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing result manifest {}", path.display()))
}

fn safe_result_file(result: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!(
            "manifest contains an unsafe result path: {}",
            relative.display()
        );
    }
    let root = std::fs::canonicalize(result)
        .with_context(|| format!("resolving result directory {}", result.display()))?;
    let path = std::fs::canonicalize(root.join(relative))
        .with_context(|| format!("resolving result file {}", relative.display()))?;
    if !path.starts_with(&root) || !path.is_file() {
        bail!(
            "result path escapes its result directory: {}",
            relative.display()
        );
    }
    Ok(path)
}

fn tool_result(result: Result<Value>) -> Value {
    match result {
        Ok(value) => json!({
            "content": [{ "type": "text", "text": value.to_string() }],
            "structuredContent": value,
            "isError": false
        }),
        Err(error) => json!({
            "content": [{ "type": "text", "text": format!("{error:#}") }],
            "isError": true
        }),
    }
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "compare_artifacts",
            "title": "Compare software artifacts",
            "description": "Compare two local files, directories, or archives and persist a patch-diff result. The output directory is transactionally replaced if it already exists.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "old_path": { "type": "string", "description": "Path to the original artifact." },
                    "new_path": { "type": "string", "description": "Path to the updated artifact." },
                    "output_path": { "type": "string", "description": "Destination result directory; an existing destination is replaced." },
                    "context": { "type": "integer", "minimum": 0, "maximum": 10000 },
                    "max_depth": { "type": "integer", "minimum": 1, "maximum": 32 },
                    "jvm": { "type": "string", "enum": ["auto", "jadx", "raw", "off"] },
                    "native": { "type": "string", "enum": ["auto", "ida", "raw", "off"] },
                    "strip_top_level": { "type": "boolean" },
                    "no_cache": { "type": "boolean" }
                },
                "required": ["old_path", "new_path", "output_path"],
                "additionalProperties": false
            },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "list_changes",
            "title": "List artifact changes",
            "description": "Read a bounded, optionally filtered page of changes from a persisted diffplus result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "result_path": { "type": "string" },
                    "status": { "type": "string", "enum": ["added", "deleted", "modified", "unchanged"] },
                    "path_prefix": { "type": "string" },
                    "include_unchanged": { "type": "boolean", "default": false },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }
                },
                "required": ["result_path"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "read_diff",
            "title": "Read a file diff",
            "description": "Read the bounded unified diff for one logical path in a persisted diffplus result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "result_path": { "type": "string" },
                    "path": { "type": "string", "description": "Logical path returned by list_changes." },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": 2097152, "default": 262144 }
                },
                "required": ["result_path", "path"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }
    ])
}

fn negotiate_version(requested: &str) -> &str {
    match requested {
        "2025-11-25" | "2025-06-18" | "2025-03-26" | "2024-11-05" => requested,
        _ => LEGACY_PROTOCOL_VERSION,
    }
}

fn is_modern_request(request: &Value) -> bool {
    request
        .pointer("/params/_meta")
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
        == Some(MODERN_PROTOCOL_VERSION)
}

fn rpc_result(id: Value, mut result: Value) -> Value {
    if let Some(result) = result.as_object_mut() {
        let metadata = result.entry("_meta").or_insert_with(|| json!({}));
        if let Some(metadata) = metadata.as_object_mut() {
            metadata.insert(
                "io.modelcontextprotocol/serverInfo".to_owned(),
                json!({
                    "name": "diffplus",
                    "version": env!("CARGO_PKG_VERSION")
                }),
            );
        }
    }
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn write_message(writer: &mut impl Write, message: Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, &message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::Cursor;

    fn base_args() -> Args {
        Args::parse_from(["diffplus", "--mcp", "--no-config"])
    }

    #[test]
    fn serves_initialize_and_tool_listing() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n"
        );
        let mut output = Vec::new();
        serve(base_args(), Cursor::new(input), &mut output).unwrap();
        let messages: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["result"]["serverInfo"]["name"], "diffplus");
        assert_eq!(messages[1]["result"]["tools"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn malformed_json_returns_parse_error() {
        let mut output = Vec::new();
        serve(base_args(), Cursor::new("not-json\n"), &mut output).unwrap();
        let message: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(message["error"]["code"], -32700);
    }

    #[test]
    fn serves_modern_stateless_discovery_and_tools() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":\"discover\",\"method\":\"server/discover\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":\"tools\",\"method\":\"tools/list\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\"}}}\n"
        );
        let mut output = Vec::new();
        serve(base_args(), Cursor::new(input), &mut output).unwrap();
        let messages: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(
            messages[0]["result"]["supportedVersions"][0],
            MODERN_PROTOCOL_VERSION
        );
        assert_eq!(messages[1]["result"]["tools"].as_array().unwrap().len(), 3);
        assert_eq!(
            messages[1]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "diffplus"
        );
    }
}
