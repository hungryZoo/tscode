use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct VisibleNode {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

#[derive(Debug, Clone)]
struct FsNode {
    path: PathBuf,
    name: String,
    is_dir: bool,
    expanded: bool,
    children: Option<Vec<FsNode>>,
}

#[derive(Debug, Clone)]
pub struct FsTree {
    root: FsNode,
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

        let mut root = FsNode {
            path: root,
            name,
            is_dir: true,
            expanded: true,
            children: None,
        };
        load_children(&mut root)?;

        Ok(Self {
            root,
            selected: 0,
            scroll: 0,
        })
    }

    pub fn visible_nodes(&self) -> Vec<VisibleNode> {
        let mut visible = Vec::new();
        flatten(&self.root, 0, &mut visible);
        visible
    }

    pub fn toggle(&mut self, path: &Path) -> Result<()> {
        if let Some(node) = find_mut(&mut self.root, path)
            && node.is_dir
        {
            if node.children.is_none() {
                load_children(node)?;
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

fn load_children(node: &mut FsNode) -> Result<()> {
    let mut children = Vec::new();
    for entry in fs::read_dir(&node.path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        children.push(FsNode {
            path,
            name,
            is_dir: file_type.is_dir(),
            expanded: false,
            children: None,
        });
    }

    children.sort_by(compare_nodes);
    node.children = Some(children);
    Ok(())
}

fn compare_nodes(a: &FsNode, b: &FsNode) -> Ordering {
    match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    }
}
