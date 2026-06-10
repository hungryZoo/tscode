use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct VisibleNode {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub size: Option<u64>,
    pub readonly: bool,
}

#[derive(Debug, Clone)]
struct FsNode {
    path: PathBuf,
    name: String,
    is_dir: bool,
    expanded: bool,
    size: Option<u64>,
    modified: Option<SystemTime>,
    readonly: bool,
    children: Option<Vec<FsNode>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplorerSortMode {
    Name,
    Type,
    Modified,
    Size,
}

impl ExplorerSortMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Type => "type",
            Self::Modified => "modified",
            Self::Size => "size",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Type,
            Self::Type => Self::Modified,
            Self::Modified => Self::Size,
            Self::Size => Self::Name,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FsTree {
    root: FsNode,
    sort_mode: ExplorerSortMode,
    pub selected: usize,
    pub scroll: usize,
}

impl FsTree {
    pub fn new(root: PathBuf) -> Result<Self> {
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| root.to_str().unwrap_or("/"))
            .to_owned();

        let metadata = fs::metadata(&root).ok();
        let mut root = FsNode {
            path: root,
            name,
            is_dir: true,
            expanded: true,
            size: None,
            modified: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            readonly: metadata
                .as_ref()
                .is_some_and(|metadata| metadata.permissions().readonly()),
            children: None,
        };
        let sort_mode = ExplorerSortMode::Name;
        load_children(&mut root, sort_mode)?;

        Ok(Self {
            root,
            sort_mode,
            selected: 0,
            scroll: 0,
        })
    }

    pub fn visible_nodes(&self) -> Vec<VisibleNode> {
        let mut visible = Vec::new();
        flatten(&self.root, 0, &mut visible);
        visible
    }

    pub fn sort_mode(&self) -> ExplorerSortMode {
        self.sort_mode
    }

    pub fn set_sort_mode(&mut self, sort_mode: ExplorerSortMode) {
        self.sort_mode = sort_mode;
        sort_loaded_children(&mut self.root, sort_mode);
        self.clamp_selection();
    }

    pub fn refresh(&mut self) -> Result<()> {
        let mut expanded = Vec::new();
        collect_expanded(&self.root, &mut expanded);
        reload_with_expanded(&mut self.root, &expanded, self.sort_mode)?;
        self.clamp_selection();
        Ok(())
    }

    pub fn toggle(&mut self, path: &Path) -> Result<()> {
        if let Some(node) = find_mut(&mut self.root, path)
            && node.is_dir
        {
            if node.children.is_none() {
                load_children(node, self.sort_mode)?;
            }
            node.expanded = !node.expanded;
        }

        self.clamp_selection();
        Ok(())
    }

    pub fn collapse(&mut self, path: &Path) {
        if let Some(node) = find_mut(&mut self.root, path)
            && node.is_dir
        {
            node.expanded = false;
        }

        self.clamp_selection();
    }

    pub fn collapse_all(&mut self) {
        collapse_descendants(&mut self.root);
        self.root.expanded = true;
        self.clamp_selection();
    }

    pub fn reveal(&mut self, path: &Path) -> Result<()> {
        expand_to_path(&mut self.root, path, self.sort_mode)?;
        self.clamp_selection();
        if let Some(index) = self
            .visible_nodes()
            .iter()
            .position(|node| node.path == path)
        {
            self.selected = index;
        }
        Ok(())
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_nodes().len();
        self.selected = self.selected.min(len.saturating_sub(1));
        self.scroll = self.scroll.min(len.saturating_sub(1));
    }
}

fn flatten(node: &FsNode, depth: usize, visible: &mut Vec<VisibleNode>) {
    visible.push(VisibleNode {
        path: node.path.clone(),
        name: node.name.clone(),
        depth,
        is_dir: node.is_dir,
        expanded: node.expanded,
        size: node.size,
        readonly: node.readonly,
    });

    if node.is_dir
        && node.expanded
        && let Some(children) = &node.children
    {
        for child in children {
            flatten(child, depth + 1, visible);
        }
    }
}

fn find_mut<'a>(node: &'a mut FsNode, path: &Path) -> Option<&'a mut FsNode> {
    if node.path == path {
        return Some(node);
    }

    if let Some(children) = &mut node.children {
        for child in children {
            if let Some(found) = find_mut(child, path) {
                return Some(found);
            }
        }
    }

    None
}

fn collapse_descendants(node: &mut FsNode) {
    if let Some(children) = &mut node.children {
        for child in children {
            child.expanded = false;
            collapse_descendants(child);
        }
    }
}

fn expand_to_path(node: &mut FsNode, path: &Path, sort_mode: ExplorerSortMode) -> Result<bool> {
    if node.path == path {
        return Ok(true);
    }
    if !node.is_dir || !path.starts_with(&node.path) {
        return Ok(false);
    }

    if node.children.is_none() {
        load_children(node, sort_mode)?;
    }
    node.expanded = true;

    if let Some(children) = &mut node.children {
        for child in children {
            if expand_to_path(child, path, sort_mode)? {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn load_children(node: &mut FsNode, sort_mode: ExplorerSortMode) -> Result<()> {
    let mut children = Vec::new();
    for entry in fs::read_dir(&node.path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let metadata = entry.metadata().ok();
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = file_type.is_dir();
        children.push(FsNode {
            path,
            name,
            is_dir,
            expanded: false,
            size: (!is_dir).then(|| metadata.as_ref().map_or(0, fs::Metadata::len)),
            modified: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            readonly: metadata
                .as_ref()
                .is_some_and(|metadata| metadata.permissions().readonly()),
            children: None,
        });
    }

    children.sort_by(|a, b| compare_nodes(a, b, sort_mode));
    node.children = Some(children);
    Ok(())
}

fn sort_loaded_children(node: &mut FsNode, sort_mode: ExplorerSortMode) {
    if let Some(children) = &mut node.children {
        children.sort_by(|a, b| compare_nodes(a, b, sort_mode));
        for child in children {
            sort_loaded_children(child, sort_mode);
        }
    }
}

fn collect_expanded(node: &FsNode, expanded: &mut Vec<PathBuf>) {
    if node.is_dir && node.expanded {
        expanded.push(node.path.clone());
        if let Some(children) = &node.children {
            for child in children {
                collect_expanded(child, expanded);
            }
        }
    }
}

fn reload_with_expanded(
    node: &mut FsNode,
    expanded: &[PathBuf],
    sort_mode: ExplorerSortMode,
) -> Result<()> {
    if !node.is_dir {
        return Ok(());
    }

    load_children(node, sort_mode)?;
    node.expanded = expanded.iter().any(|path| path == &node.path);

    if node.expanded
        && let Some(children) = &mut node.children
    {
        for child in children {
            if child.is_dir && expanded.iter().any(|path| path == &child.path) {
                reload_with_expanded(child, expanded, sort_mode)?;
            }
        }
    }

    Ok(())
}

fn compare_nodes(a: &FsNode, b: &FsNode, sort_mode: ExplorerSortMode) -> Ordering {
    match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => match sort_mode {
            ExplorerSortMode::Name => compare_names(a, b),
            ExplorerSortMode::Type => compare_types(a, b),
            ExplorerSortMode::Modified => compare_modified(a, b),
            ExplorerSortMode::Size => compare_sizes(a, b),
        },
    }
}

fn compare_names(a: &FsNode, b: &FsNode) -> Ordering {
    a.name
        .to_lowercase()
        .cmp(&b.name.to_lowercase())
        .then_with(|| a.name.cmp(&b.name))
}

fn compare_types(a: &FsNode, b: &FsNode) -> Ordering {
    node_extension(a)
        .cmp(&node_extension(b))
        .then_with(|| compare_names(a, b))
}

fn compare_modified(a: &FsNode, b: &FsNode) -> Ordering {
    b.modified
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .cmp(&a.modified.unwrap_or(SystemTime::UNIX_EPOCH))
        .then_with(|| compare_names(a, b))
}

fn compare_sizes(a: &FsNode, b: &FsNode) -> Ordering {
    b.size
        .unwrap_or(0)
        .cmp(&a.size.unwrap_or(0))
        .then_with(|| compare_names(a, b))
}

fn node_extension(node: &FsNode) -> String {
    node.path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_lowercase()
}
