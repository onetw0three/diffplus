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

pub(super) struct JadxRequest {
    pub(super) old_blob: PathBuf,
    pub(super) new_blob: PathBuf,
    pub(super) old_name: String,
    pub(super) new_name: String,
    pub(super) output: PathBuf,
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
    parents: Vec<PathBuf>,
    pending_jadx: Option<JadxRequest>,
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
            parents: Vec::new(),
            pending_jadx: None,
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

    pub(super) fn take_jadx_request(&mut self) -> Option<JadxRequest> {
        self.pending_jadx.take()
    }

    pub(super) fn finish_jadx(&mut self, request: JadxRequest, result: Result<()>) {
        self.analyzing = false;
        match result.and_then(|()| self.open_child(&request.output)) {
            Ok(()) => {}
            Err(error) => {
                self.content = Content::Message("JADX analysis failed.".into());
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    pub(super) fn has_parent(&self) -> bool {
        !self.parents.is_empty()
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
        let old_path = entry.old_path.as_deref().unwrap_or(&entry.path);
        let new_path = entry.new_path.as_deref().unwrap_or(&entry.path);
        if !is_jar_path(old_path) && !is_jar_path(new_path) {
            return Ok(());
        }
        let (Some(old_content), Some(new_content)) =
            (entry.old_content.as_deref(), entry.new_content.as_deref())
        else {
            self.error = Some(
                "JADX requires a changed JAR present on both sides; regenerate older results with this version"
                    .into(),
            );
            return Ok(());
        };

        let key = crate::scan::sha(
            format!(
                "jadx-tui-v1\n{}\n{}\n",
                entry.old_sha256.as_deref().unwrap_or("missing"),
                entry.new_sha256.as_deref().unwrap_or("missing")
            )
            .as_bytes(),
        );
        let output = self.result.join(".analysis/jadx").join(key);
        if output.join("manifest.json").is_file() {
            return self.open_child(&output);
        }

        let request = JadxRequest {
            old_blob: self.resolve_relative(old_content)?,
            new_blob: self.resolve_relative(new_content)?,
            old_name: file_name(old_path),
            new_name: file_name(new_path),
            output,
        };
        self.content = Content::Message(
            "Running JADX source analysis…\n\nThis result will be reused the next time you press Enter."
                .into(),
        );
        self.error = None;
        self.analyzing = true;
        self.pending_jadx = Some(request);
        Ok(())
    }

    fn open_child(&mut self, result: &Path) -> Result<()> {
        let mut child = Self::load(result)?;
        child.parents = std::mem::take(&mut self.parents);
        child.parents.push(self.result.clone());
        *self = child;
        Ok(())
    }

    fn open_parent(&mut self) -> Result<()> {
        let Some(result) = self.parents.pop() else {
            return Ok(());
        };
        let remaining = std::mem::take(&mut self.parents);
        let mut parent = Self::load(&result)?;
        parent.parents = remaining;
        *self = parent;
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
        self.vertical_scroll = 0;
        self.horizontal_scroll = 0;
        self.error = None;
    }

    fn refresh_content(&mut self) {
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
        let resolved = self.resolve_relative(relative)?;
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

    fn resolve_relative(&self, relative: &str) -> Result<PathBuf> {
        let relative = Path::new(relative);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
        {
            bail!("unsafe result path: {}", relative.display());
        }
        let path = self.result.join(relative);
        let root = std::fs::canonicalize(&self.result)?;
        let resolved = std::fs::canonicalize(&path)
            .with_context(|| format!("resolving {}", path.display()))?;
        if !resolved.starts_with(root) {
            bail!("result path escapes its directory: {}", path.display());
        }
        Ok(resolved)
    }
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
        let request = app.take_jadx_request().unwrap();
        assert_eq!(request.old_name, "example-1.jar");
        assert_eq!(request.new_name, "example-2.jar");
        assert_eq!(
            std::fs::read(request.old_blob).unwrap(),
            b"old jar".as_slice()
        );
    }
}
