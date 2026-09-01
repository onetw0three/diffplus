use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "artifact-diff",
    version,
    about = "Compare directories and archives by semantic file contents"
)]
pub struct Args {
    /// Original input. Required unless --view is used.
    #[arg(required_unless_present = "view")]
    pub old: Option<PathBuf>,
    /// New input. Required unless --view is used.
    #[arg(required_unless_present = "view")]
    pub new: Option<PathBuf>,
    /// Open an existing result directory in the terminal UI.
    #[arg(long, value_name = "RESULT", conflicts_with_all = ["old", "new"])]
    pub view: Option<PathBuf>,
    /// Open the terminal UI after generating the comparison.
    #[arg(long)]
    pub tui: bool,
    #[arg(short, long, default_value = "result")]
    pub output: PathBuf,
    #[arg(long, value_enum, default_value_t = Color::Auto)]
    pub color: Color,
    #[arg(long, default_value_t = 3)]
    pub context: usize,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    pub max_file_size: u64,
    /// Maximum total uncompressed bytes visited per input artifact.
    #[arg(long, default_value_t = 1024 * 1024 * 1024)]
    pub max_expanded_size: u64,
    /// Archive expansion levels, including the top-level archive (minimum 1).
    #[arg(long, default_value_t = 1)]
    pub max_depth: usize,
    #[arg(long, value_enum, default_value_t = JvmMode::Auto)]
    pub jvm: JvmMode,
    #[arg(long, default_value = "jadx")]
    pub jadx_path: PathBuf,
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
    /// Parent directory for ephemeral extracted-content workspaces.
    #[arg(long)]
    pub workspace_dir: Option<PathBuf>,
    #[arg(long)]
    pub no_cache: bool,
    #[arg(long, value_enum, default_value_t = NativeMode::Auto)]
    pub native: NativeMode,
    #[arg(long)]
    pub ida_path: Option<PathBuf>,
    #[arg(long)]
    pub diaphora_script: Option<PathBuf>,
    /// Directory containing Diaphora's diaphora.py and diaphora_ida.py.
    #[arg(long)]
    pub diaphora_path: Option<PathBuf>,
    /// Python interpreter used for the database-comparison adapter phase.
    #[arg(long, default_value = "python3")]
    pub python_path: PathBuf,
    #[arg(long)]
    pub strip_top_level: bool,
    /// Suppress progress messages; errors and the final summary are unchanged.
    #[arg(long)]
    pub quiet: bool,
}

impl Args {
    pub(crate) fn old_input(&self) -> &std::path::Path {
        self.old
            .as_deref()
            .expect("clap requires OLD in comparison mode")
    }

    pub(crate) fn new_input(&self) -> &std::path::Path {
        self.new
            .as_deref()
            .expect("clap requires NEW in comparison mode")
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Color {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum JvmMode {
    Auto,
    Jadx,
    Raw,
    Off,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum NativeMode {
    Auto,
    Ida,
    Raw,
    Off,
}
