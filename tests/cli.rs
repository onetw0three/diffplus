use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use std::fs;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(&new).unwrap();
    (temp, old, new)
}

#[test]
fn writes_clean_git_style_results() {
    let (temp, old, new) = fixture();
    fs::write(old.join("deleted.txt"), "deleted\n").unwrap();
    fs::write(new.join("added.txt"), "added\n").unwrap();
    let output = temp.path().join("result");
    let workspace = temp.path().join("workspaces");
    fs::create_dir_all(&output).unwrap();
    fs::write(output.join("stale.diff"), "stale").unwrap();

    Command::cargo_bin("artifact-diff")
        .unwrap()
        .arg(&old)
        .arg(&new)
        .arg("-o")
        .arg(&output)
        .args(["--jvm", "raw"])
        .arg("--workspace-dir")
        .arg(&workspace)
        .assert()
        .code(1);

    assert!(!output.join("stale.diff").exists());
    let added = fs::read(output.join("diffs/added.txt.diff")).unwrap();
    assert!(!added.contains(&0));
    let added = String::from_utf8(added).unwrap();
    assert!(added.contains("--- /dev/null"));
    assert!(added.contains("+++ b/added.txt"));
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["schema_version"], 3);
    let added_entry = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "added.txt")
        .unwrap();
    let blob = added_entry["new_content"].as_str().unwrap();
    assert_eq!(fs::read_to_string(output.join(blob)).unwrap(), "added\n");
    assert!(fs::read_dir(&workspace).unwrap().next().is_none());
}

#[test]
fn identical_archives_reuse_one_content_scan() {
    use std::io::Write;

    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old.zip");
    let new = temp.path().join("new.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&old).unwrap());
    for index in 0..100 {
        writer
            .start_file(
                format!("files/{index}.txt"),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writeln!(writer, "value {index}").unwrap();
    }
    writer.finish().unwrap();
    fs::copy(&old, &new).unwrap();

    Command::cargo_bin("artifact-diff")
        .unwrap()
        .args(["--jvm", "raw", "--output"])
        .arg(temp.path().join("result"))
        .arg(&old)
        .arg(&new)
        .assert()
        .code(0)
        .stderr(predicates::str::contains("reusing the old content scan"));
}

#[test]
fn default_mode_retains_changed_nested_jars_for_on_demand_analysis() {
    use std::io::{Cursor, Write};

    fn jar_bytes(class: &[u8]) -> Vec<u8> {
        let mut jar = zip::ZipWriter::new(Cursor::new(Vec::new()));
        jar.start_file(
            "example/Main.class",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        jar.write_all(class).unwrap();
        jar.finish().unwrap().into_inner()
    }

    fn write_tar(path: &std::path::Path, member: &str, bytes: &[u8]) {
        let mut archive = tar::Builder::new(fs::File::create(path).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, member, bytes).unwrap();
        archive.finish().unwrap();
    }

    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old.tar");
    let new = temp.path().join("new.tar");
    let output = temp.path().join("result");
    write_tar(
        &old,
        "lib/example-26.0.3.76224.jar",
        &jar_bytes(b"old class"),
    );
    write_tar(
        &new,
        "lib/example-26.0.4-PO-4560.76507.jar",
        &jar_bytes(b"new class"),
    );

    Command::cargo_bin("artifact-diff")
        .unwrap()
        .arg(&old)
        .arg(&new)
        .arg("--output")
        .arg(&output)
        .assert()
        .code(1)
        .stderr(predicates::str::contains("running JADX").not());

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    let entries = manifest["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"], "modified");
    assert_eq!(entries[0]["renamed"], true);
    assert!(output
        .join(entries[0]["old_content"].as_str().unwrap())
        .is_file());
    assert!(output
        .join(entries[0]["new_content"].as_str().unwrap())
        .is_file());
}

#[test]
fn default_mode_retains_unmatched_native_binaries_for_manual_analysis() {
    fn write_tar(path: &std::path::Path, member: &str, bytes: &[u8]) {
        let mut archive = tar::Builder::new(fs::File::create(path).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, member, bytes).unwrap();
        archive.finish().unwrap();
    }

    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old.tar");
    let new = temp.path().join("new.tar");
    let output = temp.path().join("result");
    write_tar(&old, "bin/legacy-service", b"\x7fELFold native");
    write_tar(&new, "bin/replacement-service", b"\x7fELFnew native");

    Command::cargo_bin("artifact-diff")
        .unwrap()
        .arg(&old)
        .arg(&new)
        .arg("--output")
        .arg(&output)
        .assert()
        .code(1);

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    let entries = manifest["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| {
        entry["old_content"]
            .as_str()
            .or_else(|| entry["new_content"].as_str())
            .is_some_and(|blob| output.join(blob).is_file())
    }));
}

#[test]
fn different_tars_scan_concurrently_and_render_range_backed_diff() {
    fn write_tar(path: &std::path::Path, content: &[u8]) {
        let mut builder = tar::Builder::new(fs::File::create(path).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "src/value.txt", content)
            .unwrap();
        builder.finish().unwrap();
    }

    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old.tar");
    let new = temp.path().join("new.tar");
    let output = temp.path().join("result");
    write_tar(&old, b"before\n");
    write_tar(&new, b"after\n");

    let mut command = Command::cargo_bin("artifact-diff").unwrap();
    let assertion = command
        .args(["--jvm", "raw", "--output"])
        .arg(&output)
        .arg(&old)
        .arg(&new)
        .assert()
        .code(1);
    if std::thread::available_parallelism().is_ok_and(|parallelism| parallelism.get() > 1) {
        assert!(String::from_utf8_lossy(&assertion.get_output().stderr)
            .contains("scanning both uncompressed TAR inputs concurrently"));
    }
    let diff = fs::read_to_string(output.join("diffs/src/value.txt.diff")).unwrap();
    assert!(diff.contains("-before"));
    assert!(diff.contains("+after"));
}

#[test]
fn fatal_input_error_uses_exit_two() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("artifact-diff")
        .unwrap()
        .arg(temp.path().join("missing-old"))
        .arg(temp.path().join("missing-new"))
        .assert()
        .code(2);
}

#[test]
fn explicit_config_supplies_defaults_and_cli_overrides_them() {
    let (temp, old, new) = fixture();
    fs::write(old.join("value.txt"), "old\n").unwrap();
    fs::write(new.join("value.txt"), "new\n").unwrap();
    let configured_output = temp.path().join("configured-result");
    let cli_output = temp.path().join("cli-result");
    let config = temp.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "output = {:?}\njvm = 'raw'\nmax_depth = 1\n",
            configured_output
        ),
    )
    .unwrap();

    Command::cargo_bin("artifact-diff")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg(&old)
        .arg(&new)
        .assert()
        .code(1);
    assert!(configured_output.join("manifest.json").is_file());

    Command::cargo_bin("artifact-diff")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("--output")
        .arg(&cli_output)
        .arg(&old)
        .arg(&new)
        .assert()
        .code(1);
    assert!(cli_output.join("manifest.json").is_file());
}

#[test]
fn explicit_missing_config_is_an_error() {
    let (temp, old, new) = fixture();
    Command::cargo_bin("artifact-diff")
        .unwrap()
        .arg("--config")
        .arg(temp.path().join("missing.toml"))
        .arg(&old)
        .arg(&new)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("configuration file not found"));
}

#[test]
fn view_mode_does_not_require_input_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("artifact-diff")
        .unwrap()
        .arg("--view")
        .arg(temp.path())
        .assert()
        .code(2)
        .stderr(predicates::str::contains("interactive stdin and stdout"));
}

#[cfg(unix)]
#[test]
fn native_pipeline_writes_function_diff_and_metadata() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old.bin");
    let new = temp.path().join("new.bin");
    fs::write(&old, b"\x7fELFold").unwrap();
    fs::write(&new, b"\x7fELFnew").unwrap();

    let ida = temp.path().join("fake-ida");
    let counter = temp.path().join("ida-count");
    fs::write(
        &ida,
        "#!/bin/sh\necho x >> \"$ARTIFACT_DIFF_COUNTER\"\npython3 -c 'import json,os; r=json.load(open(os.environ[\"ARTIFACT_DIFF_REQUEST\"])); open(r[\"export_database\"],\"wb\").write(b\"sqlite\")'\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&ida).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&ida, permissions).unwrap();

    let adapter = temp.path().join("adapter.py");
    fs::write(
        &adapter,
        "import json,os\nr=json.load(open(os.environ['ARTIFACT_DIFF_REQUEST']))\nf={'stable_id':'parse','old_address':1,'new_address':2,'old_name':'parse','new_name':'parse','status':'modified','similarity':0.9,'match_category':'partial','match_reason':'test','old_pseudocode':'int parse() { return 1; }','new_pseudocode':'int parse() { return 2; }'}\njson.dump({'protocol_version':1,'functions':[f]},open(r['output'],'w'))\n",
    )
    .unwrap();
    let diaphora = temp.path().join("diaphora");
    fs::create_dir(&diaphora).unwrap();
    fs::write(diaphora.join("diaphora.py"), "").unwrap();
    fs::write(diaphora.join("diaphora_ida.py"), "").unwrap();
    let output = temp.path().join("result");
    let cache = temp.path().join("cache");

    for _ in 0..2 {
        Command::cargo_bin("artifact-diff")
            .unwrap()
            .env("ARTIFACT_DIFF_COUNTER", &counter)
            .args(["--native", "ida", "--ida-path"])
            .arg(&ida)
            .arg("--diaphora-script")
            .arg(&adapter)
            .arg("--diaphora-path")
            .arg(&diaphora)
            .arg("--cache-dir")
            .arg(&cache)
            .arg("--output")
            .arg(&output)
            .arg(&old)
            .arg(&new)
            .assert()
            .code(1);
    }

    let diff = fs::read_to_string(output.join("diffs/functions/parse.c.diff")).unwrap();
    assert!(diff.contains("return 1"));
    assert!(diff.contains("return 2"));
    let metadata = fs::read_to_string(output.join("native-functions.json")).unwrap();
    assert!(metadata.contains("\"similarity\": 0.9"));
    assert_eq!(fs::read_to_string(counter).unwrap().lines().count(), 2);
}

#[cfg(unix)]
#[test]
fn identical_native_inputs_skip_ida() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old.bin");
    let new = temp.path().join("new.bin");
    fs::write(&old, b"\x7fELFsame").unwrap();
    fs::copy(&old, &new).unwrap();
    let ida = temp.path().join("fake-ida");
    fs::write(&ida, "#!/bin/sh\nexit 99\n").unwrap();
    let mut permissions = fs::metadata(&ida).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&ida, permissions).unwrap();
    let adapter = temp.path().join("adapter.py");
    fs::write(&adapter, "raise SystemExit(99)\n").unwrap();
    let diaphora = temp.path().join("diaphora");
    fs::create_dir(&diaphora).unwrap();
    fs::write(diaphora.join("diaphora.py"), "").unwrap();
    fs::write(diaphora.join("diaphora_ida.py"), "").unwrap();

    Command::cargo_bin("artifact-diff")
        .unwrap()
        .args(["--native", "ida", "--ida-path"])
        .arg(&ida)
        .arg("--diaphora-script")
        .arg(&adapter)
        .arg("--diaphora-path")
        .arg(&diaphora)
        .arg("--output")
        .arg(temp.path().join("result"))
        .arg(&old)
        .arg(&new)
        .assert()
        .code(0)
        .stderr(predicates::str::contains("skipping IDA/Diaphora"));
}
