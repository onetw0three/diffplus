use crate::model::{Manifest, ManifestEntry};
use anyhow::{bail, Context, Result};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use similar::{DiffTag, TextDiff};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path, PathBuf},
};

const MAX_VIEW_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CONSOLIDATED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ViewMode {
    SideBySide,
    Unified,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragTarget {
    MainDivider,
    DiffDivider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowKind {
    Equal,
    Added,
    Deleted,
    Modified,
    Header,
    Hunk,
}

pub(super) struct DiffCell {
    pub(super) number: Option<usize>,
    pub(super) text: String,
    pub(super) kind: RowKind,
}

pub(super) struct DiffRow {
    pub(super) old: DiffCell,
    pub(super) new: DiffCell,
}

pub(super) struct UnifiedLine {
    pub(super) text: String,
    pub(super) kind: RowKind,
}

pub(super) enum Content {
    SideBySide(Vec<DiffRow>),
    Unified(Vec<UnifiedLine>),
    Message(String),
}

#[derive(Clone)]
pub(super) struct TreeNode {
    pub(super) label: String,
    pub(super) path: String,
    pub(super) depth: usize,
    pub(super) directory: bool,
    pub(super) expanded: bool,
    pub(super) entry: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AnalyzerKind {
    Text,
    Jadx,
    Ilspy,
    Ida,
}

impl AnalyzerKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Jadx => "JADX",
            Self::Ilspy => "ILSpy",
            Self::Ida => "IDA/Diaphora",
        }
    }

    fn directory(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Jadx => "jadx",
            Self::Ilspy => "ilspy",
            Self::Ida => "ida",
        }
    }
}

pub(super) struct AnalysisRequest {
    pub(super) kind: AnalyzerKind,
    pub(super) old_blob: PathBuf,
    pub(super) new_blob: PathBuf,
    pub(super) old_name: String,
    pub(super) new_name: String,
    old_digest: String,
    new_digest: String,
    pub(super) output: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactSide {
    Old,
    New,
}

#[derive(Clone)]
struct MarkedFile {
    side: ArtifactSide,
    blob: PathBuf,
    path: String,
    digest: String,
    kind: AnalyzerKind,
}

#[derive(Default)]
struct Branch {
    directories: BTreeMap<String, Branch>,
    files: Vec<(String, usize)>,
}

pub(super) struct App {
    pub(super) manifest: Manifest,
    result: PathBuf,
    pub(super) query: String,
    pub(super) searching: bool,
    filters: [bool; 4],
    pub(super) selected: usize,
    collapsed: HashSet<String>,
    pub(super) mode: ViewMode,
    pub(super) sidebar_percent: u16,
    pub(super) diff_percent: u16,
    drag_target: Option<DragTarget>,
    pub(super) vertical_scroll: u16,
    pub(super) horizontal_scroll: u16,
    pub(super) content: Content,
    pub(super) error: Option<String>,
    active_analysis: Option<PathBuf>,
    analysis_names: Option<(String, String)>,
    parent: Option<Box<App>>,
    marked: Option<MarkedFile>,
    manual_results: BTreeMap<String, PathBuf>,
    pending_analysis: Option<AnalysisRequest>,
    pub(super) analyzing: bool,
}

impl App {
    pub(super) fn load(result: &Path) -> Result<Self> {
        let manifest_path = result.join("manifest.json");
        let bytes = std::fs::read(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;
        if !matches!(manifest.schema_version, 2 | 3) {
            bail!(
                "unsupported manifest schema {}; this viewer supports schemas 2 and 3",
                manifest.schema_version
            );
        }
        let mut app = Self {
            manifest,
            result: result.to_path_buf(),
            query: String::new(),
            searching: false,
            filters: [true, true, true, false],
            selected: 0,
            collapsed: HashSet::new(),
            mode: ViewMode::SideBySide,
            sidebar_percent: 34,
            diff_percent: 50,
            drag_target: None,
            vertical_scroll: 0,
            horizontal_scroll: 0,
            content: Content::Message(String::new()),
            error: None,
            active_analysis: None,
            analysis_names: None,
            parent: None,
            marked: None,
            manual_results: BTreeMap::new(),
            pending_analysis: None,
            analyzing: false,
        };
        app.select_first_file();
        app.refresh_content();
        Ok(app)
    }

    pub(super) fn visible_nodes(&self) -> Vec<TreeNode> {
        let mut root = Branch::default();
        for (index, entry) in self.manifest.entries.iter().enumerate() {
            if !self.entry_visible(entry) {
                continue;
            }
            insert_entry(&mut root, &entry.path, index);
        }
        let mut nodes = Vec::new();
        flatten_branch(&root, "", 0, &self.collapsed, &mut nodes);
        nodes
    }

    pub(super) fn selected_entry(&self) -> Option<&ManifestEntry> {
        self.visible_nodes()
            .get(self.selected)
            .and_then(|node| node.entry)
            .and_then(|index| self.manifest.entries.get(index))
    }

    pub(super) fn tree_scroll(&self, viewport_height: usize) -> usize {
        self.selected.saturating_sub(viewport_height / 2)
    }

    pub(super) fn filter_enabled(&self, index: usize) -> bool {
        self.filters[index]
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }
        if self.searching {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.query.pop();
                    self.after_tree_change();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.clear();
                    self.after_tree_change();
                }
                KeyCode::Char(character) => {
                    self.query.push(character);
                    self.after_tree_change();
                }
                _ => {}
            }
            return Ok(false);
        }
        if self.analyzing {
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.vertical_scroll = self.vertical_scroll.saturating_sub(3)
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.vertical_scroll = self.vertical_scroll.saturating_add(3)
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char('m') => self.toggle_mark()?,
            KeyCode::Enter => self.activate_or_analyze()?,
            KeyCode::Char(' ') | KeyCode::Right => self.activate_selected(false),
            KeyCode::Left => self.activate_selected(true),
            KeyCode::Backspace => self.open_parent()?,
            KeyCode::PageDown => self.vertical_scroll = self.vertical_scroll.saturating_add(20),
            KeyCode::PageUp => self.vertical_scroll = self.vertical_scroll.saturating_sub(20),
            KeyCode::Char('J') => self.vertical_scroll = self.vertical_scroll.saturating_add(1),
            KeyCode::Char('K') => self.vertical_scroll = self.vertical_scroll.saturating_sub(1),
            KeyCode::Home => {
                self.vertical_scroll = 0;
                self.horizontal_scroll = 0;
            }
            KeyCode::Char('[') => self.horizontal_scroll = self.horizontal_scroll.saturating_sub(4),
            KeyCode::Char(']') => self.horizontal_scroll = self.horizontal_scroll.saturating_add(4),
            KeyCode::Tab | KeyCode::Char('u') => {
                self.mode = match self.mode {
                    ViewMode::SideBySide => ViewMode::Unified,
                    ViewMode::Unified => ViewMode::SideBySide,
                };
                self.refresh_content();
            }
            KeyCode::Char(value @ '1'..='4') => {
                let index = value as usize - '1' as usize;
                self.filters[index] = !self.filters[index];
                self.after_tree_change();
            }
            _ => {}
        }
        Ok(false)
    }

    pub(super) fn take_analysis_request(&mut self) -> Option<AnalysisRequest> {
        self.pending_analysis.take()
    }

    pub(super) fn finish_analysis(&mut self, request: AnalysisRequest, result: Result<()>) {
        self.analyzing = false;
        if result.is_ok() {
            self.active_analysis = Some(request.output.clone());
            self.analysis_names = Some((request.old_name.clone(), request.new_name.clone()));
            self.manual_results
                .insert(request.old_digest.clone(), request.output.clone());
            self.manual_results
                .insert(request.new_digest.clone(), request.output.clone());
        }
        match result.and_then(|()| self.open_child(&request.output)) {
            Ok(()) => {}
            Err(error) => {
                self.content =
                    Content::Message(format!("{} analysis failed.", request.kind.label()));
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    pub(super) fn showing_analysis(&self) -> bool {
        self.active_analysis.is_some()
    }

    pub(super) fn diff_names(&self) -> (&str, &str) {
        self.analysis_names
            .as_ref()
            .map(|(old, new)| (old.as_str(), new.as_str()))
            .unwrap_or((&self.manifest.old.name, &self.manifest.new.name))
    }

    pub(super) fn has_parent(&self) -> bool {
        self.parent.is_some()
    }

    pub(super) fn is_marked_entry(&self, index: usize) -> bool {
        let Some(marked) = &self.marked else {
            return false;
        };
        self.manifest
            .entries
            .get(index)
            .is_some_and(|entry| match marked.side {
                ArtifactSide::Old => {
                    entry.old_sha256.as_deref() == Some(marked.digest.as_str())
                        && entry.old_path.as_deref() == Some(marked.path.as_str())
                }
                ArtifactSide::New => {
                    entry.new_sha256.as_deref() == Some(marked.digest.as_str())
                        && entry.new_path.as_deref() == Some(marked.path.as_str())
                }
            })
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent, width: u16, height: u16) {
        let sidebar_width = percent_of(width, self.sidebar_percent);
        let over_sidebar = mouse.column < sidebar_width;
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left)
                if near_divider(mouse.column, sidebar_width)
                    && mouse.row >= 2
                    && mouse.row < height.saturating_sub(1) =>
            {
                self.drag_target = Some(DragTarget::MainDivider);
            }
            MouseEventKind::Down(MouseButton::Left)
                if self.mode == ViewMode::SideBySide
                    && near_divider(
                        mouse.column,
                        sidebar_width.saturating_add(percent_of(
                            width.saturating_sub(sidebar_width),
                            self.diff_percent,
                        )),
                    )
                    && mouse.row >= 5 =>
            {
                self.drag_target = Some(DragTarget::DiffDivider);
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag_target {
                Some(DragTarget::MainDivider) if width > 0 => {
                    self.sidebar_percent =
                        ((u32::from(mouse.column) * 100) / u32::from(width)).clamp(20, 60) as u16;
                }
                Some(DragTarget::DiffDivider) => {
                    let editor_width = width.saturating_sub(sidebar_width);
                    if editor_width > 0 {
                        let local = mouse.column.saturating_sub(sidebar_width);
                        self.diff_percent = ((u32::from(local) * 100) / u32::from(editor_width))
                            .clamp(20, 80) as u16;
                    }
                }
                _ => {}
            },
            MouseEventKind::Up(MouseButton::Left) => self.drag_target = None,
            MouseEventKind::ScrollUp if over_sidebar => self.move_selection(-3),
            MouseEventKind::ScrollDown if over_sidebar => self.move_selection(3),
            MouseEventKind::ScrollUp => {
                self.vertical_scroll = self.vertical_scroll.saturating_sub(3)
            }
            MouseEventKind::ScrollDown => {
                self.vertical_scroll = self.vertical_scroll.saturating_add(3)
            }
            MouseEventKind::Down(MouseButton::Left)
                if over_sidebar && mouse.row >= 8 && mouse.row < height.saturating_sub(2) =>
            {
                let viewport_height = height.saturating_sub(9) as usize;
                let index = self.tree_scroll(viewport_height) + usize::from(mouse.row - 8);
                if index < self.visible_nodes().len() {
                    self.selected = index;
                    self.reset_view();
                    self.refresh_content();
                    if self
                        .visible_nodes()
                        .get(index)
                        .is_some_and(|node| node.directory)
                    {
                        self.activate_selected(false);
                    }
                }
            }
            _ => {}
        }
    }

    fn entry_visible(&self, entry: &ManifestEntry) -> bool {
        let status_index = match entry.status.as_str() {
            "modified" => 0,
            "renamed" => 0,
            "added" => 1,
            "deleted" => 2,
            "unchanged" => 3,
            _ => return false,
        };
        self.filters[status_index]
            && (self.query.is_empty()
                || [
                    Some(entry.path.as_str()),
                    entry.old_path.as_deref(),
                    entry.new_path.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|path| path.to_lowercase().contains(&self.query.to_lowercase())))
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.visible_nodes().len();
        if count == 0 {
            self.selected = 0;
            self.refresh_content();
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        self.reset_view();
        self.refresh_content();
    }

    fn activate_selected(&mut self, collapse_only: bool) {
        let nodes = self.visible_nodes();
        let Some(node) = nodes.get(self.selected) else {
            return;
        };
        if node.directory {
            self.active_analysis = None;
            if !collapse_only && self.collapsed.remove(&node.path) {
                // Right/Enter toggles a collapsed directory open.
            } else {
                self.collapsed.insert(node.path.clone());
            }
            let count = self.visible_nodes().len();
            self.selected = self.selected.min(count.saturating_sub(1));
            self.refresh_content();
        }
    }

    fn activate_or_analyze(&mut self) -> Result<()> {
        let nodes = self.visible_nodes();
        let Some(node) = nodes.get(self.selected) else {
            return Ok(());
        };
        if node.directory {
            self.activate_selected(false);
            return Ok(());
        }

        let Some(entry) = node
            .entry
            .and_then(|index| self.manifest.entries.get(index))
        else {
            return Ok(());
        };

        if let Some(marked) = self.marked.clone() {
            let Some(current) = self.one_sided_candidate(entry)? else {
                self.error = Some("Select an unmatched added or deleted comparable file".into());
                return Ok(());
            };
            if current.side == marked.side {
                self.error = Some("The marked files must come from opposite artifact sides".into());
                return Ok(());
            }
            let (old, new) = if marked.side == ArtifactSide::Old {
                (marked, current)
            } else {
                (current, marked)
            };
            return self.queue_analysis(old, new);
        }

        let Some((old, new)) = self.paired_candidates(entry)? else {
            if entry.old_content.is_some() || entry.new_content.is_some() {
                self.error = Some(
                    "Press m on an unmatched file, select its counterpart, then press Enter".into(),
                );
            }
            return Ok(());
        };
        self.queue_analysis(old, new)
    }

    fn toggle_mark(&mut self) -> Result<()> {
        let Some(entry) = self
            .visible_nodes()
            .get(self.selected)
            .and_then(|node| node.entry)
            .and_then(|index| self.manifest.entries.get(index))
        else {
            return Ok(());
        };
        let Some(candidate) = self.one_sided_candidate(entry)? else {
            self.error =
                Some("Only unmatched text files, JARs, or native binaries can be marked".into());
            return Ok(());
        };
        if self.marked.as_ref().is_some_and(|marked| {
            marked.side == candidate.side && marked.digest == candidate.digest
        }) {
            self.marked = None;
        } else {
            self.marked = Some(candidate);
        }
        self.error = None;
        Ok(())
    }

    fn queue_analysis(&mut self, old: MarkedFile, new: MarkedFile) -> Result<()> {
        if old.kind != new.kind {
            self.error = Some(format!(
                "Analyzer mismatch: {} uses {}, while {} uses {}",
                old.path,
                old.kind.label(),
                new.path,
                new.kind.label()
            ));
            return Ok(());
        }
        let output = analysis_output_path(&self.result, old.kind, &old.digest, &new.digest);
        if output.join("manifest.json").is_file() {
            self.active_analysis = Some(output.clone());
            self.analysis_names = Some((file_name(&old.path), file_name(&new.path)));
            self.manual_results
                .insert(old.digest.clone(), output.clone());
            self.manual_results
                .insert(new.digest.clone(), output.clone());
            return self.open_child(&output);
        }

        let request = AnalysisRequest {
            kind: old.kind,
            old_blob: old.blob,
            new_blob: new.blob,
            old_name: file_name(&old.path),
            new_name: file_name(&new.path),
            old_digest: old.digest,
            new_digest: new.digest,
            output,
        };
        self.content = Content::Message(format!(
            "Running {} analysis…\n\nThis result will be reused the next time you press Enter.",
            request.kind.label()
        ));
        self.error = None;
        self.analyzing = true;
        self.marked = None;
        self.pending_analysis = Some(request);
        Ok(())
    }

    fn paired_candidates(&self, entry: &ManifestEntry) -> Result<Option<(MarkedFile, MarkedFile)>> {
        let (Some(old_content), Some(new_content)) =
            (entry.old_content.as_deref(), entry.new_content.as_deref())
        else {
            return Ok(None);
        };
        let old_path = entry.old_path.as_deref().unwrap_or(&entry.path);
        let new_path = entry.new_path.as_deref().unwrap_or(&entry.path);
        let old_blob = self.resolve_relative(old_content)?;
        let new_blob = self.resolve_relative(new_content)?;
        let Some(old_kind) = analyzer_kind(old_path, &old_blob)? else {
            return Ok(None);
        };
        let Some(new_kind) = analyzer_kind(new_path, &new_blob)? else {
            return Ok(None);
        };
        Ok(Some((
            MarkedFile {
                side: ArtifactSide::Old,
                blob: old_blob,
                path: old_path.to_owned(),
                digest: entry.old_sha256.clone().unwrap_or_default(),
                kind: old_kind,
            },
            MarkedFile {
                side: ArtifactSide::New,
                blob: new_blob,
                path: new_path.to_owned(),
                digest: entry.new_sha256.clone().unwrap_or_default(),
                kind: new_kind,
            },
        )))
    }

    fn one_sided_candidate(&self, entry: &ManifestEntry) -> Result<Option<MarkedFile>> {
        let (side, content, path, digest) =
            match (entry.old_content.as_deref(), entry.new_content.as_deref()) {
                (Some(content), None) => (
                    ArtifactSide::Old,
                    content,
                    entry.old_path.as_deref().unwrap_or(&entry.path),
                    entry.old_sha256.as_deref().unwrap_or_default(),
                ),
                (None, Some(content)) => (
                    ArtifactSide::New,
                    content,
                    entry.new_path.as_deref().unwrap_or(&entry.path),
                    entry.new_sha256.as_deref().unwrap_or_default(),
                ),
                _ => return Ok(None),
            };
        let blob = self.resolve_relative(content)?;
        let kind = match analyzer_kind(path, &blob)? {
            Some(kind) => kind,
            None if entry.kind == "text" => AnalyzerKind::Text,
            None => return Ok(None),
        };
        Ok(Some(MarkedFile {
            side,
            blob,
            path: path.to_owned(),
            digest: digest.to_owned(),
            kind,
        }))
    }

    fn open_child(&mut self, result: &Path) -> Result<()> {
        let mut child = Self::load(result)?;
        std::mem::swap(self, &mut child);
        self.parent = Some(Box::new(child));
        Ok(())
    }

    fn open_parent(&mut self) -> Result<()> {
        let Some(parent) = self.parent.take() else {
            return Ok(());
        };
        *self = *parent;
        self.refresh_content();
        Ok(())
    }

    fn after_tree_change(&mut self) {
        self.selected = 0;
        self.select_first_file();
        self.reset_view();
        self.refresh_content();
    }

    fn select_first_file(&mut self) {
        if let Some(index) = self.visible_nodes().iter().position(|node| !node.directory) {
            self.selected = index;
        }
    }

    fn reset_view(&mut self) {
        self.active_analysis = None;
        self.analysis_names = None;
        self.vertical_scroll = 0;
        self.horizontal_scroll = 0;
        self.error = None;
    }

    fn refresh_content(&mut self) {
        if self.active_analysis.is_none() {
            if let Some(result) = self.cached_analysis_result() {
                self.analysis_names = load_manifest(&result)
                    .ok()
                    .map(|manifest| (manifest.old.name, manifest.new.name));
                self.active_analysis = Some(result);
            }
        }
        if let Some(result) = self.active_analysis.as_deref() {
            let loaded = match self.mode {
                ViewMode::SideBySide => load_consolidated_side_by_side(result),
                ViewMode::Unified => load_consolidated_unified(result),
            };
            match loaded {
                Ok(content) => {
                    self.content = content;
                    self.error = None;
                }
                Err(error) => {
                    self.content = Content::Message("Unable to display the analyzer diff.".into());
                    self.error = Some(format!("{error:#}"));
                }
            }
            return;
        }
        let Some(entry_index) = self
            .visible_nodes()
            .get(self.selected)
            .and_then(|node| node.entry)
        else {
            self.content = Content::Message("Select a file to inspect its comparison.".into());
            return;
        };
        let entry = &self.manifest.entries[entry_index];
        let loaded = match self.mode {
            ViewMode::SideBySide => self.load_side_by_side(entry),
            ViewMode::Unified => self.load_unified(entry),
        };
        match loaded {
            Ok(content) => {
                self.content = content;
                self.error = None;
            }
            Err(error) => {
                self.content = Content::Message("Unable to display this entry.".into());
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    fn cached_analysis_result(&self) -> Option<PathBuf> {
        let entry = self.selected_entry()?;
        if let Some(result) = entry
            .old_sha256
            .as_ref()
            .or(entry.new_sha256.as_ref())
            .and_then(|digest| self.manual_results.get(digest))
        {
            return Some(result.clone());
        }
        let old_digest = entry.old_sha256.as_deref()?;
        let new_digest = entry.new_sha256.as_deref()?;
        [AnalyzerKind::Text, AnalyzerKind::Jadx, AnalyzerKind::Ida]
            .into_iter()
            .map(|kind| analysis_output_path(&self.result, kind, old_digest, new_digest))
            .find(|result| result.join("manifest.json").is_file())
    }

    fn load_side_by_side(&self, entry: &ManifestEntry) -> Result<Content> {
        if entry.kind != "text" {
            return Ok(Content::Message(format!(
                "Binary entry ({})\n\nold: {}\nnew: {}",
                entry.status,
                entry.old_sha256.as_deref().unwrap_or("—"),
                entry.new_sha256.as_deref().unwrap_or("—")
            )));
        }
        if entry.status == "unchanged" {
            return Ok(Content::Message(
                "Unchanged content is not copied into the result.\nUse status filter 4 to hide unchanged entries."
                    .into(),
            ));
        }
        let old = self.read_optional(entry.old_content.as_deref())?;
        let new = self.read_optional(entry.new_content.as_deref())?;
        Ok(Content::SideBySide(aligned_diff(&old, &new)))
    }

    fn load_unified(&self, entry: &ManifestEntry) -> Result<Content> {
        let Some(path) = entry.diff.as_deref() else {
            return Ok(Content::Message("No diff is stored for this entry.".into()));
        };
        let text = self.read_relative(path)?;
        Ok(Content::Unified(
            text.lines()
                .map(|line| UnifiedLine {
                    kind: classify_unified(line),
                    text: line.to_owned(),
                })
                .collect(),
        ))
    }

    fn read_optional(&self, path: Option<&str>) -> Result<String> {
        path.map_or_else(|| Ok(String::new()), |path| self.read_relative(path))
    }

    fn read_relative(&self, relative: &str) -> Result<String> {
        read_relative_from(&self.result, relative)
    }

    fn resolve_relative(&self, relative: &str) -> Result<PathBuf> {
        resolve_relative_from(&self.result, relative)
    }
}

fn load_consolidated_side_by_side(result: &Path) -> Result<Content> {
    let manifest = load_manifest(result)?;
    let mut rows = Vec::new();
    let mut total_bytes = 0;
    for entry in manifest
        .entries
        .iter()
        .filter(|entry| entry.status != "unchanged" && entry.kind == "text")
    {
        let old = read_optional_from(result, entry.old_content.as_deref())?;
        let new = read_optional_from(result, entry.new_content.as_deref())?;
        total_bytes += old.len() + new.len();
        if total_bytes > MAX_CONSOLIDATED_BYTES {
            bail!("consolidated comparison exceeds 64 MiB");
        }
        rows.push(DiffRow {
            old: DiffCell {
                number: None,
                text: format!("--- {}", entry.old_path.as_deref().unwrap_or("/dev/null")),
                kind: RowKind::Header,
            },
            new: DiffCell {
                number: None,
                text: format!("+++ {}", entry.new_path.as_deref().unwrap_or("/dev/null")),
                kind: RowKind::Header,
            },
        });
        rows.extend(aligned_diff(&old, &new));
        rows.push(blank_diff_row());
    }
    if rows.is_empty() {
        Ok(Content::Message(
            "The comparison produced no changed text representations for this pair.".into(),
        ))
    } else {
        Ok(Content::SideBySide(rows))
    }
}

fn load_consolidated_unified(result: &Path) -> Result<Content> {
    let manifest = load_manifest(result)?;
    let mut lines = Vec::new();
    let mut total_bytes = 0;
    for entry in manifest
        .entries
        .iter()
        .filter(|entry| entry.status != "unchanged")
    {
        let Some(diff) = entry.diff.as_deref() else {
            continue;
        };
        let text = read_relative_from(result, diff)?;
        total_bytes += text.len();
        if total_bytes > MAX_CONSOLIDATED_BYTES {
            bail!("consolidated comparison exceeds 64 MiB");
        }
        lines.extend(text.lines().map(|line| UnifiedLine {
            kind: classify_unified(line),
            text: line.to_owned(),
        }));
        lines.push(UnifiedLine {
            text: String::new(),
            kind: RowKind::Equal,
        });
    }
    if lines.is_empty() {
        Ok(Content::Message(
            "The comparison produced no changed text representations for this pair.".into(),
        ))
    } else {
        Ok(Content::Unified(lines))
    }
}

fn load_manifest(result: &Path) -> Result<Manifest> {
    let path = result.join("manifest.json");
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

fn read_optional_from(result: &Path, relative: Option<&str>) -> Result<String> {
    relative.map_or_else(
        || Ok(String::new()),
        |path| read_relative_from(result, path),
    )
}

fn read_relative_from(result: &Path, relative: &str) -> Result<String> {
    let resolved = resolve_relative_from(result, relative)?;
    let size = std::fs::metadata(&resolved)?.len();
    if size > MAX_VIEW_BYTES {
        bail!(
            "{} is {} MiB; the viewer limit is {} MiB",
            resolved.display(),
            size / 1024 / 1024,
            MAX_VIEW_BYTES / 1024 / 1024
        );
    }
    let bytes = std::fs::read(&resolved)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn resolve_relative_from(result: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("unsafe result path: {}", relative.display());
    }
    let path = result.join(relative);
    let root = std::fs::canonicalize(result)?;
    let resolved =
        std::fs::canonicalize(&path).with_context(|| format!("resolving {}", path.display()))?;
    if !resolved.starts_with(root) {
        bail!("result path escapes its directory: {}", path.display());
    }
    Ok(resolved)
}

fn blank_diff_row() -> DiffRow {
    DiffRow {
        old: DiffCell {
            number: None,
            text: String::new(),
            kind: RowKind::Equal,
        },
        new: DiffCell {
            number: None,
            text: String::new(),
            kind: RowKind::Equal,
        },
    }
}

fn analysis_output_path(
    result: &Path,
    kind: AnalyzerKind,
    old_digest: &str,
    new_digest: &str,
) -> PathBuf {
    let protocol = match kind {
        AnalyzerKind::Text => "text-tui-v1",
        AnalyzerKind::Jadx => "jadx-tui-v1",
        AnalyzerKind::Ilspy => "ilspy-tui-v1",
        AnalyzerKind::Ida => "ida-tui-v1",
    };
    let key = crate::scan::sha(format!("{protocol}\n{old_digest}\n{new_digest}\n").as_bytes());
    result.join(".analysis").join(kind.directory()).join(key)
}

fn analyzer_kind(path: &str, blob: &Path) -> Result<Option<AnalyzerKind>> {
    if is_jar_path(path) {
        return Ok(Some(AnalyzerKind::Jadx));
    }
    if crate::classify::is_dotnet_pe(blob)? {
        return Ok(Some(AnalyzerKind::Ilspy));
    }
    use std::io::Read;
    let mut magic = [0_u8; 4];
    let read = std::fs::File::open(blob)?.read(&mut magic)?;
    let magic = &magic[..read];
    let native = crate::classify::is_native_magic(magic);
    Ok(native.then_some(AnalyzerKind::Ida))
}

fn is_jar_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn percent_of(value: u16, percent: u16) -> u16 {
    ((u32::from(value) * u32::from(percent)) / 100) as u16
}

fn near_divider(column: u16, divider: u16) -> bool {
    column.abs_diff(divider) <= 1
}

fn insert_entry(root: &mut Branch, path: &str, index: usize) {
    let mut parts = path.split('/').peekable();
    let mut branch = root;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            branch.files.push((part.to_owned(), index));
            break;
        }
        branch = branch.directories.entry(part.to_owned()).or_default();
    }
}

fn flatten_branch(
    branch: &Branch,
    parent: &str,
    depth: usize,
    collapsed: &HashSet<String>,
    output: &mut Vec<TreeNode>,
) {
    for (name, child) in &branch.directories {
        let path = if parent.is_empty() {
            name.clone()
        } else {
            format!("{parent}/{name}")
        };
        let expanded = !collapsed.contains(&path);
        output.push(TreeNode {
            label: name.clone(),
            path: path.clone(),
            depth,
            directory: true,
            expanded,
            entry: None,
        });
        if expanded {
            flatten_branch(child, &path, depth + 1, collapsed, output);
        }
    }
    for (name, index) in &branch.files {
        let path = if parent.is_empty() {
            name.clone()
        } else {
            format!("{parent}/{name}")
        };
        output.push(TreeNode {
            label: name.clone(),
            path,
            depth,
            directory: false,
            expanded: false,
            entry: Some(*index),
        });
    }
}

fn aligned_diff(old: &str, new: &str) -> Vec<DiffRow> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let diff = TextDiff::from_lines(old, new);
    let mut rows = Vec::new();
    for operation in diff.ops() {
        let old_range = operation.old_range();
        let new_range = operation.new_range();
        let count = old_range.len().max(new_range.len());
        let kinds = match operation.tag() {
            DiffTag::Equal => (RowKind::Equal, RowKind::Equal),
            DiffTag::Delete => (RowKind::Deleted, RowKind::Equal),
            DiffTag::Insert => (RowKind::Equal, RowKind::Added),
            DiffTag::Replace => (RowKind::Modified, RowKind::Modified),
        };
        for offset in 0..count {
            rows.push(DiffRow {
                old: make_cell(&old_lines, old_range.start + offset, old_range.end, kinds.0),
                new: make_cell(&new_lines, new_range.start + offset, new_range.end, kinds.1),
            });
        }
    }
    rows
}

fn make_cell(lines: &[&str], index: usize, end: usize, kind: RowKind) -> DiffCell {
    if index < end {
        DiffCell {
            number: Some(index + 1),
            text: lines.get(index).copied().unwrap_or_default().to_owned(),
            kind,
        }
    } else {
        DiffCell {
            number: None,
            text: String::new(),
            kind,
        }
    }
}

fn classify_unified(line: &str) -> RowKind {
    if line.starts_with("@@") {
        RowKind::Hunk
    } else if line.starts_with("+++") || line.starts_with("---") {
        RowKind::Header
    } else if line.starts_with('+') {
        RowKind::Added
    } else if line.starts_with('-') {
        RowKind::Deleted
    } else {
        RowKind::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_fixture() -> tempfile::TempDir {
        let result = tempfile::tempdir().unwrap();
        std::fs::create_dir(result.path().join("blobs")).unwrap();
        std::fs::create_dir_all(result.path().join("diffs/src")).unwrap();
        std::fs::write(result.path().join("blobs/old"), "before\n").unwrap();
        std::fs::write(result.path().join("blobs/new"), "after\n").unwrap();
        std::fs::write(
            result.path().join("diffs/src/main.rs.diff"),
            "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-before\n+after\n",
        )
        .unwrap();
        std::fs::write(
            result.path().join("manifest.json"),
            r#"{
              "schema_version": 2,
              "old": {"name": "old", "sha256": "a"},
              "new": {"name": "new", "sha256": "b"},
              "stats": {"added": 0, "deleted": 0, "modified": 1, "unchanged": 1},
              "entries": [
                {"path":"src/main.rs","kind":"text","status":"modified","diff":"diffs/src/main.rs.diff","old_sha256":"old","new_sha256":"new","old_content":"blobs/old","new_content":"blobs/new"},
                {"path":"src/same.rs","kind":"text","status":"unchanged","diff":null,"old_sha256":"same","new_sha256":"same","old_content":null,"new_content":null}
              ]
            }"#,
        )
        .unwrap();
        result
    }

    fn jar_result_fixture() -> tempfile::TempDir {
        let result = tempfile::tempdir().unwrap();
        std::fs::create_dir(result.path().join("blobs")).unwrap();
        std::fs::write(result.path().join("blobs/old-jar"), b"old jar").unwrap();
        std::fs::write(result.path().join("blobs/new-jar"), b"new jar").unwrap();
        std::fs::write(
            result.path().join("manifest.json"),
            r#"{
              "schema_version": 3,
              "old": {"name": "old", "sha256": "a"},
              "new": {"name": "new", "sha256": "b"},
              "stats": {"added": 0, "deleted": 0, "modified": 1, "unchanged": 0, "renamed": 1},
              "entries": [{
                "path":"lib/example-2.jar",
                "old_path":"lib/example-1.jar",
                "new_path":"lib/example-2.jar",
                "kind":"binary",
                "status":"modified",
                "renamed":true,
                "diff":"diffs/example.diff",
                "old_sha256":"old",
                "new_sha256":"new",
                "old_size":7,
                "new_size":7,
                "old_content":"blobs/old-jar",
                "new_content":"blobs/new-jar"
              }]
            }"#,
        )
        .unwrap();
        result
    }

    fn unmatched_jar_result_fixture() -> tempfile::TempDir {
        let result = tempfile::tempdir().unwrap();
        std::fs::create_dir(result.path().join("blobs")).unwrap();
        std::fs::write(result.path().join("blobs/old-only"), b"old jar").unwrap();
        std::fs::write(result.path().join("blobs/new-only"), b"new jar").unwrap();
        std::fs::write(
            result.path().join("manifest.json"),
            r#"{
              "schema_version":3,
              "old":{"name":"old","sha256":"a"},
              "new":{"name":"new","sha256":"b"},
              "stats":{"added":1,"deleted":1,"modified":0,"unchanged":0,"renamed":0},
              "entries":[
                {"path":"a-old.jar","old_path":"a-old.jar","new_path":null,"kind":"binary","status":"deleted","renamed":false,"diff":null,"old_sha256":"old-only","new_sha256":null,"old_content":"blobs/old-only","new_content":null},
                {"path":"z-new.jar","old_path":null,"new_path":"z-new.jar","kind":"binary","status":"added","renamed":false,"diff":null,"old_sha256":null,"new_sha256":"new-only","old_content":null,"new_content":"blobs/new-only"}
              ]
            }"#,
        )
        .unwrap();
        result
    }

    fn unmatched_text_result_fixture() -> tempfile::TempDir {
        let result = tempfile::tempdir().unwrap();
        std::fs::create_dir(result.path().join("blobs")).unwrap();
        std::fs::write(result.path().join("blobs/old-text"), "before\n").unwrap();
        std::fs::write(result.path().join("blobs/new-text"), "after\n").unwrap();
        std::fs::write(
            result.path().join("manifest.json"),
            r#"{
              "schema_version":3,
              "old":{"name":"old","sha256":"a"},
              "new":{"name":"new","sha256":"b"},
              "stats":{"added":1,"deleted":1,"modified":0,"unchanged":0,"renamed":0},
              "entries":[
                {"path":"legacy.txt","old_path":"legacy.txt","new_path":null,"kind":"text","status":"deleted","renamed":false,"diff":null,"old_sha256":"old-text","new_sha256":null,"old_size":7,"new_size":null,"old_content":"blobs/old-text","new_content":null},
                {"path":"replacement.txt","old_path":null,"new_path":"replacement.txt","kind":"text","status":"added","renamed":false,"diff":null,"old_sha256":null,"new_sha256":"new-text","old_size":null,"new_size":6,"old_content":null,"new_content":"blobs/new-text"}
              ]
            }"#,
        )
        .unwrap();
        result
    }

    fn add_cached_jadx_result(parent: &Path) {
        let key = crate::scan::sha(b"jadx-tui-v1\nold\nnew\n");
        let result = parent.join(".analysis/jadx").join(key);
        std::fs::create_dir_all(result.join("blobs")).unwrap();
        std::fs::create_dir_all(result.join("diffs/sources")).unwrap();
        std::fs::write(result.join("blobs/source-old"), "return 1;\n").unwrap();
        std::fs::write(result.join("blobs/source-new"), "return 2;\n").unwrap();
        std::fs::write(
            result.join("diffs/sources/Main.java.diff"),
            "--- a/sources/Main.java\n+++ b/sources/Main.java\n-return 1;\n+return 2;\n",
        )
        .unwrap();
        std::fs::write(
            result.join("manifest.json"),
            r#"{
              "schema_version":3,
              "old":{"name":"example-1.jar","sha256":"old"},
              "new":{"name":"example-2.jar","sha256":"new"},
              "stats":{"added":0,"deleted":0,"modified":1,"unchanged":0,"renamed":0},
              "entries":[{"path":"sources/Main.java","old_path":"sources/Main.java","new_path":"sources/Main.java","kind":"text","status":"modified","renamed":false,"diff":"diffs/sources/Main.java.diff","old_sha256":"source-old","new_sha256":"source-new","old_content":"blobs/source-old","new_content":"blobs/source-new"}]
            }"#,
        )
        .unwrap();
    }

    #[test]
    fn aligns_replaced_lines() {
        let rows = aligned_diff("one\ntwo\n", "one\nthree\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].old.text, "two");
        assert_eq!(rows[1].new.text, "three");
        assert_eq!(rows[1].old.kind, RowKind::Modified);
    }

    #[test]
    fn rejects_parent_components() {
        let path = Path::new("../secret");
        assert!(path
            .components()
            .any(|part| matches!(part, Component::ParentDir)));
    }

    #[test]
    fn loads_tree_and_changed_content_lazily() {
        let result = result_fixture();
        let app = App::load(result.path()).unwrap();
        let nodes = app.visible_nodes();
        assert_eq!(nodes.len(), 2);
        assert!(nodes[0].directory);
        assert_eq!(nodes[1].path, "src/main.rs");
        assert!(matches!(&app.content, Content::SideBySide(rows) if rows.len() == 1));
    }

    #[test]
    fn toggles_unified_view_and_unchanged_filter() {
        let result = result_fixture();
        let mut app = App::load(result.path()).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        assert!(matches!(&app.content, Content::Unified(lines) if lines.len() == 5));
        app.handle_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.visible_nodes().len(), 3);
    }

    #[test]
    fn space_toggles_selected_directory() {
        let result = result_fixture();
        let mut app = App::load(result.path()).unwrap();
        app.selected = 0;
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.visible_nodes().len(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.visible_nodes().len(), 2);
    }

    #[test]
    fn mouse_wheel_scrolls_diff_pane() {
        let result = result_fixture();
        let mut app = App::load(result.path()).unwrap();
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 70,
                row: 12,
                modifiers: KeyModifiers::NONE,
            },
            100,
            30,
        );
        assert_eq!(app.vertical_scroll, 3);
    }

    #[test]
    fn mouse_drag_resizes_main_and_diff_panels() {
        let result = result_fixture();
        let mut app = App::load(result.path()).unwrap();
        let mouse = |kind, column| MouseEvent {
            kind,
            column,
            row: 12,
            modifiers: KeyModifiers::NONE,
        };

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 34), 100, 30);
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50), 100, 30);
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 50), 100, 30);
        assert_eq!(app.sidebar_percent, 50);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 75), 100, 30);
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 90), 100, 30);
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 90), 100, 30);
        assert_eq!(app.diff_percent, 80);
    }

    #[test]
    fn enter_queues_jadx_for_a_changed_jar() {
        let result = jar_result_fixture();
        let mut app = App::load(result.path()).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        assert!(app.analyzing);
        let request = app.take_analysis_request().unwrap();
        assert_eq!(request.old_name, "example-1.jar");
        assert_eq!(request.new_name, "example-2.jar");
        assert_eq!(
            std::fs::read(request.old_blob).unwrap(),
            b"old jar".as_slice()
        );
    }

    #[test]
    fn consolidates_all_jadx_changes_into_one_diff() {
        let result = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(result.path().join("blobs")).unwrap();
        std::fs::create_dir_all(result.path().join("diffs/sources")).unwrap();
        std::fs::write(result.path().join("blobs/a-old"), "before a\n").unwrap();
        std::fs::write(result.path().join("blobs/a-new"), "after a\n").unwrap();
        std::fs::write(result.path().join("blobs/b-old"), "before b\n").unwrap();
        std::fs::write(result.path().join("blobs/b-new"), "after b\n").unwrap();
        std::fs::write(
            result.path().join("diffs/sources/A.java.diff"),
            "--- a/sources/A.java\n+++ b/sources/A.java\n-before a\n+after a\n",
        )
        .unwrap();
        std::fs::write(
            result.path().join("diffs/sources/B.java.diff"),
            "--- a/sources/B.java\n+++ b/sources/B.java\n-before b\n+after b\n",
        )
        .unwrap();
        std::fs::write(
            result.path().join("manifest.json"),
            r#"{
              "schema_version":3,
              "old":{"name":"old.jar","sha256":"a"},
              "new":{"name":"new.jar","sha256":"b"},
              "stats":{"added":0,"deleted":0,"modified":2,"unchanged":0,"renamed":0},
              "entries":[
                {"path":"sources/A.java","old_path":"sources/A.java","new_path":"sources/A.java","kind":"text","status":"modified","renamed":false,"diff":"diffs/sources/A.java.diff","old_sha256":"a-old","new_sha256":"a-new","old_content":"blobs/a-old","new_content":"blobs/a-new"},
                {"path":"sources/B.java","old_path":"sources/B.java","new_path":"sources/B.java","kind":"text","status":"modified","renamed":false,"diff":"diffs/sources/B.java.diff","old_sha256":"b-old","new_sha256":"b-new","old_content":"blobs/b-old","new_content":"blobs/b-new"}
              ]
            }"#,
        )
        .unwrap();

        let side_by_side = load_consolidated_side_by_side(result.path()).unwrap();
        assert!(matches!(side_by_side, Content::SideBySide(rows) if
            rows.iter().any(|row| row.old.text == "--- sources/A.java") &&
            rows.iter().any(|row| row.old.text == "--- sources/B.java")));
        let unified = load_consolidated_unified(result.path()).unwrap();
        assert!(matches!(unified, Content::Unified(lines) if
            lines.iter().any(|line| line.text == "-before a") &&
            lines.iter().any(|line| line.text == "+after b")));
    }

    #[test]
    fn selection_consolidates_cached_jadx_and_enter_opens_child() {
        let result = jar_result_fixture();
        add_cached_jadx_result(result.path());
        let mut app = App::load(result.path()).unwrap();

        assert!(app.showing_analysis());
        assert!(matches!(&app.content, Content::SideBySide(rows) if
            rows.iter().any(|row| row.old.text == "return 1;") &&
            rows.iter().any(|row| row.new.text == "return 2;")));
        assert_eq!(app.diff_names(), ("example-1.jar", "example-2.jar"));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(app.has_parent());
        assert_eq!(app.manifest.old.name, "example-1.jar");

        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        assert!(!app.has_parent());
        assert_eq!(app.manifest.old.name, "old");
        assert!(app.showing_analysis());
        assert_eq!(app.diff_names(), ("example-1.jar", "example-2.jar"));
    }

    #[test]
    fn marks_and_pairs_unmatched_jars_for_analysis() {
        let result = unmatched_jar_result_fixture();
        let mut app = App::load(result.path()).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.is_marked_entry(0));

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        let request = app.take_analysis_request().unwrap();
        assert_eq!(request.kind, AnalyzerKind::Jadx);
        assert_eq!(request.old_name, "a-old.jar");
        assert_eq!(request.new_name, "z-new.jar");
    }

    #[test]
    fn marks_and_pairs_unmatched_text_files() {
        let result = unmatched_text_result_fixture();
        let mut app = App::load(result.path()).unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.is_marked_entry(0));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        let request = app.take_analysis_request().unwrap();
        assert_eq!(request.kind, AnalyzerKind::Text);
        assert_eq!(request.old_name, "legacy.txt");
        assert_eq!(request.new_name, "replacement.txt");
        crate::core::run_text_diff(
            &request.old_blob,
            &request.new_blob,
            &request.old_name,
            &request.new_name,
            &request.output,
        )
        .unwrap();
        app.finish_analysis(request, Ok(()));

        assert!(app.has_parent());
        assert_eq!(app.manifest.entries.len(), 1);
        assert_eq!(app.manifest.entries[0].status, "modified");
        assert_eq!(
            app.manifest.entries[0].old_path.as_deref(),
            Some("legacy.txt")
        );
        assert_eq!(
            app.manifest.entries[0].new_path.as_deref(),
            Some("replacement.txt")
        );
    }

    #[test]
    fn recognizes_native_binaries_by_magic_not_filename() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"\x7fELFpayload").unwrap();
        assert_eq!(
            analyzer_kind("renamed-without-extension", file.path()).unwrap(),
            Some(AnalyzerKind::Ida)
        );
    }
}
