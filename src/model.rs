use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Content {
    File(PathBuf),
    Range {
        source: PathBuf,
        offset: u64,
        size: u64,
    },
}

impl Content {
    pub(crate) fn open(&self) -> std::io::Result<Box<dyn Read>> {
        match self {
            Self::File(path) => Ok(Box::new(std::fs::File::open(path)?)),
            Self::Range {
                source,
                offset,
                size,
            } => {
                let mut file = std::fs::File::open(source)?;
                file.seek(SeekFrom::Start(*offset))?;
                Ok(Box::new(file.take(*size)))
            }
        }
    }

    pub(crate) fn read(&self) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.open()?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub(crate) fn copy_to(&self, destination: &std::path::Path) -> std::io::Result<u64> {
        let mut source = self.open()?;
        let mut destination = std::fs::File::create(destination)?;
        std::io::copy(&mut source, &mut destination)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) path: String,
    pub(crate) content: Content,
    pub(crate) digest: String,
    pub(crate) size: u64,
    pub(crate) kind: &'static str,
}

#[derive(Default)]
pub(crate) struct Artifact {
    pub(crate) name: String,
    pub(crate) digest: String,
    pub(crate) entries: BTreeMap<String, Entry>,
    // Keeps extracted and generated files alive through diff generation.
    pub(crate) _workspace: Option<tempfile::TempDir>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Manifest {
    pub(crate) schema_version: u32,
    pub(crate) old: ArtifactInfo,
    pub(crate) new: ArtifactInfo,
    pub(crate) stats: Stats,
    pub(crate) entries: Vec<ManifestEntry>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ArtifactInfo {
    pub(crate) name: String,
    pub(crate) sha256: String,
}

#[derive(Default, Deserialize, Serialize, Clone)]
pub(crate) struct Stats {
    pub(crate) added: usize,
    pub(crate) deleted: usize,
    pub(crate) modified: usize,
    pub(crate) unchanged: usize,
    #[serde(default)]
    pub(crate) renamed: usize,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ManifestEntry {
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) old_path: Option<String>,
    #[serde(default)]
    pub(crate) new_path: Option<String>,
    pub(crate) kind: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) renamed: bool,
    pub(crate) diff: Option<String>,
    pub(crate) old_sha256: Option<String>,
    pub(crate) new_sha256: Option<String>,
    #[serde(default)]
    pub(crate) old_size: Option<u64>,
    #[serde(default)]
    pub(crate) new_size: Option<u64>,
    pub(crate) old_content: Option<String>,
    pub(crate) new_content: Option<String>,
}
