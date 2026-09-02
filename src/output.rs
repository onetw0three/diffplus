use anyhow::{Context, Result};
use std::path::Path;

use crate::model::Manifest;

pub(crate) struct OutputTransaction {
    temporary: tempfile::TempDir,
    staged: std::path::PathBuf,
    destination: std::path::PathBuf,
}

impl OutputTransaction {
    pub(crate) fn new(destination: &Path) -> Result<Self> {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temporary = tempfile::Builder::new()
            .prefix(".diffplus-")
            .tempdir_in(parent)?;
        let staged = temporary.path().join("result");
        std::fs::create_dir(&staged)?;
        Ok(Self {
            temporary,
            staged,
            destination: destination.to_path_buf(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.staged
    }

    pub(crate) fn commit(self) -> Result<()> {
        let backup = self.temporary.path().join("previous-result");
        let had_previous = std::fs::symlink_metadata(&self.destination).is_ok();
        if had_previous {
            std::fs::rename(&self.destination, &backup).with_context(|| {
                format!("backing up existing output {}", self.destination.display())
            })?;
        }
        if let Err(error) = std::fs::rename(&self.staged, &self.destination) {
            if had_previous {
                std::fs::rename(&backup, &self.destination).with_context(|| {
                    format!("restoring existing output after replacement failed: {error}")
                })?;
            }
            return Err(error)
                .with_context(|| format!("replacing output {}", self.destination.display()));
        }
        self.temporary.close()?;
        Ok(())
    }
}

pub(crate) fn write_results(output: &Path, manifest: &Manifest, summary: &str) -> Result<()> {
    std::fs::create_dir_all(output)?;
    std::fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(manifest)?,
    )?;
    std::fs::write(output.join("summary.txt"), summary)?;
    Ok(())
}
