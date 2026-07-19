use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;
use parking_lot::RwLock;

use crate::{
    encoding,
    error::{ApiError, ApiResult},
    fileio::{canonical_existing, ensure_within},
    model::{SearchHit, SearchRequest, WorkspaceEntry, WorkspaceSnapshot},
};

pub struct WorkspaceStore {
    root: RwLock<Option<PathBuf>>,
}

impl WorkspaceStore {
    pub fn new() -> Self {
        Self {
            root: RwLock::new(None),
        }
    }

    pub fn open(&self, path: &Path) -> ApiResult<WorkspaceSnapshot> {
        let root = canonical_existing(path)?;
        if !root.is_dir() {
            return Err(ApiError::new(
                "not_a_directory",
                "The selected path is not a directory.",
            ));
        }
        *self.root.write() = Some(root.clone());
        self.snapshot(&root)
    }

    pub fn current_root(&self) -> Option<PathBuf> {
        self.root.read().clone()
    }

    pub fn refresh(&self) -> ApiResult<Option<WorkspaceSnapshot>> {
        self.root
            .read()
            .clone()
            .map(|root| self.snapshot(&root))
            .transpose()
    }

    pub fn search(&self, request: SearchRequest) -> ApiResult<Vec<SearchHit>> {
        let root = canonical_existing(Path::new(&request.root))?;
        let active = self.require_root()?;
        if active != root {
            return Err(ApiError::new(
                "workspace_mismatch",
                "Search is restricted to the active workspace.",
            ));
        }
        let query = if request.case_sensitive {
            request.query.clone()
        } else {
            request.query.to_lowercase()
        };
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = request.limit.unwrap_or(500).clamp(1, 2_000) as usize;
        let mut hits = Vec::new();
        let mut builder = WalkBuilder::new(&root);
        builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .follow_links(false)
            .filter_entry(|entry| !is_heavy_directory(entry.path()));
        for result in builder.build() {
            let Ok(entry) = result else { continue };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) || !is_markdown(entry.path()) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() > 20 * 1024 * 1024 {
                continue;
            }
            let absolute = entry.path().to_string_lossy().into_owned();
            let relative = entry
                .path()
                .strip_prefix(&root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .into_owned();
            let mut local_hits = Vec::new();
            let mut utf8_failed = false;
            if let Ok(file) = fs::File::open(entry.path()) {
                for (index, line) in BufReader::new(file).lines().enumerate() {
                    match line {
                        Ok(line) => {
                            if let Some(hit) = search_line(
                                &absolute,
                                &relative,
                                index,
                                &line,
                                &query,
                                request.case_sensitive,
                            ) {
                                local_hits.push(hit);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                            utf8_failed = true;
                            break;
                        }
                        Err(_) => break,
                    }
                }
            }
            if utf8_failed {
                local_hits.clear();
                let Ok(bytes) = fs::read(entry.path()) else {
                    continue;
                };
                let Ok(decoded) = encoding::decode(&bytes) else {
                    continue;
                };
                for (index, line) in decoded.content.lines().enumerate() {
                    if let Some(hit) = search_line(
                        &absolute,
                        &relative,
                        index,
                        line,
                        &query,
                        request.case_sensitive,
                    ) {
                        local_hits.push(hit);
                    }
                }
            }
            for hit in local_hits {
                hits.push(hit);
                if hits.len() >= limit {
                    return Ok(hits);
                }
            }
        }
        Ok(hits)
    }

    pub fn create_entry(
        &self,
        parent: &Path,
        name: &str,
        is_dir: bool,
    ) -> ApiResult<WorkspaceSnapshot> {
        validate_name(name)?;
        let root = self.require_root()?;
        let parent = ensure_within(&root, parent)?;
        let target = ensure_within(&root, &parent.join(name))?;
        if target.exists() {
            return Err(ApiError::new(
                "already_exists",
                "A file with this name already exists.",
            ));
        }
        if is_dir {
            fs::create_dir(&target)
                .map_err(|error| ApiError::io("Unable to create the folder", error))?;
        } else {
            fs::write(&target, b"")
                .map_err(|error| ApiError::io("Unable to create the document", error))?;
        }
        self.snapshot(&root)
    }

    pub fn rename_entry(&self, path: &Path, new_name: &str) -> ApiResult<WorkspaceSnapshot> {
        validate_name(new_name)?;
        let root = self.require_root()?;
        let source = ensure_within(&root, path)?;
        let target = ensure_within(
            &root,
            &source
                .parent()
                .ok_or_else(|| ApiError::new("invalid_path", "Cannot rename the workspace root."))?
                .join(new_name),
        )?;
        if target.exists() {
            return Err(ApiError::new(
                "already_exists",
                "A file with this name already exists.",
            ));
        }
        fs::rename(source, target)
            .map_err(|error| ApiError::io("Unable to rename the entry", error))?;
        self.snapshot(&root)
    }

    pub fn trash_entry(&self, path: &Path) -> ApiResult<WorkspaceSnapshot> {
        let root = self.require_root()?;
        let target = ensure_within(&root, path)?;
        if target == root {
            return Err(ApiError::new(
                "invalid_path",
                "The workspace root cannot be deleted.",
            ));
        }
        trash::delete(&target)
            .map_err(|error| ApiError::io("Unable to move the entry to the recycle bin", error))?;
        self.snapshot(&root)
    }

    fn snapshot(&self, root: &Path) -> ApiResult<WorkspaceSnapshot> {
        let mut entries = Vec::new();
        let mut builder = WalkBuilder::new(root);
        builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .follow_links(false)
            .sort_by_file_name(|left, right| left.cmp(right))
            .filter_entry(|entry| !is_heavy_directory(entry.path()));
        for result in builder.build() {
            let Ok(entry) = result else { continue };
            if entry.path() == root || entry.file_type().is_some_and(|kind| kind.is_symlink()) {
                continue;
            }
            let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
            if !is_dir && !is_visible_file(entry.path()) {
                continue;
            }
            let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
            entries.push(WorkspaceEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path().to_string_lossy().into_owned(),
                is_dir,
                depth: relative.components().count().saturating_sub(1) as u32,
            });
        }
        Ok(WorkspaceSnapshot {
            root: root.to_string_lossy().into_owned(),
            name: root
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("Workspace")
                .to_string(),
            entries,
        })
    }

    fn require_root(&self) -> ApiResult<PathBuf> {
        self.current_root().ok_or_else(|| {
            ApiError::new(
                "workspace_not_open",
                "Open a workspace before changing its files.",
            )
        })
    }
}

fn search_line(
    path: &str,
    relative_path: &str,
    index: usize,
    line: &str,
    query: &str,
    case_sensitive: bool,
) -> Option<SearchHit> {
    let haystack = if case_sensitive {
        line.to_string()
    } else {
        line.to_lowercase()
    };
    let byte_column = haystack.find(query)?;
    let column = haystack[..byte_column].chars().count() as u32 + 1;
    Some(SearchHit {
        path: path.to_string(),
        relative_path: relative_path.to_string(),
        line: index as u32 + 1,
        column,
        preview: line.trim().chars().take(240).collect(),
    })
}

fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd")
    )
}

fn is_visible_file(path: &Path) -> bool {
    is_markdown(path)
        || matches!(
            path.extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "pdf")
        )
}

fn is_heavy_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some(".git" | "node_modules" | "target" | ".idea" | ".vscode")
    )
}

fn validate_name(name: &str) -> ApiResult<()> {
    if name.trim().is_empty()
        || name.contains(['/', '\\'])
        || name == "."
        || name == ".."
        || name.chars().any(|value| "<>:\"|?*".contains(value))
    {
        return Err(ApiError::new(
            "invalid_name",
            "The file name is not valid on Windows.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_markdown_and_skips_node_modules() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("note.md"), "hello InkFlow\n").unwrap();
        fs::create_dir(temp.path().join("node_modules")).unwrap();
        fs::write(temp.path().join("node_modules/hidden.md"), "InkFlow").unwrap();
        let store = WorkspaceStore::new();
        store.open(temp.path()).unwrap();
        let hits = store
            .search(SearchRequest {
                root: temp.path().to_string_lossy().into_owned(),
                query: "inkflow".into(),
                case_sensitive: false,
                limit: None,
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn search_requires_an_active_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::new();
        let result = store.search(SearchRequest {
            root: temp.path().to_string_lossy().into_owned(),
            query: "inkflow".into(),
            case_sensitive: false,
            limit: None,
        });
        assert!(result.is_err());
    }
}
