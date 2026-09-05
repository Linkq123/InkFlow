use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;
use parking_lot::RwLock;

use crate::{
    data_lock::lock_path_mutations,
    encoding,
    error::{ApiError, ApiResult},
    fileio::{
        AtomicWriteOutcome, atomic_create_if_absent, canonical_existing, ensure_within,
        is_symbolic_link_or_junction,
    },
    model::{SearchHit, SearchRequest, WorkspaceEntry, WorkspaceSnapshot},
};

const MAX_SEARCH_FILE_BYTES: u64 = 20 * 1024 * 1024;

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
        let root = self.select_root(path)?;
        self.snapshot(&root)
    }

    /// Selects a workspace without eagerly walking its contents. Streaming
    /// callers use this so the first search result is not delayed by an
    /// otherwise discarded tree snapshot.
    pub fn select_root(&self, path: &Path) -> ApiResult<PathBuf> {
        let root = canonical_existing(path)?;
        if !root.is_dir() {
            return Err(ApiError::new(
                "not_a_directory",
                "The selected path is not a directory.",
            ));
        }
        *self.root.write() = Some(root.clone());
        Ok(root)
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
        let mut hits = Vec::new();
        self.search_with(request, |hit| {
            hits.push(hit.clone());
            Ok(())
        })?;
        Ok(hits)
    }

    /// Searches incrementally and invokes `on_hit` for each matching line once
    /// the current file's encoding has been determined. This lets CLI JSONL
    /// consumers process a hit without waiting for the rest of that file or the
    /// workspace, while avoiding provisional UTF-8 hits from legacy files.
    pub fn search_with<F>(&self, request: SearchRequest, mut on_hit: F) -> ApiResult<usize>
    where
        F: FnMut(&SearchHit) -> ApiResult<()>,
    {
        self.search_with_control(request, &mut on_hit, || false)
    }

    pub fn search_with_control<F, C>(
        &self,
        request: SearchRequest,
        mut on_hit: F,
        mut is_cancelled: C,
    ) -> ApiResult<usize>
    where
        F: FnMut(&SearchHit) -> ApiResult<()>,
        C: FnMut() -> bool,
    {
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
            return Ok(0);
        }
        let limit = request.limit.unwrap_or(500).clamp(1, 2_000) as usize;
        let mut hit_count = 0;
        let mut builder = WalkBuilder::new(&root);
        let filter_root = root.clone();
        builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .follow_links(false)
            .filter_entry(move |entry| {
                entry.path() == filter_root
                    || (!is_heavy_directory(entry.path()) && !is_reparse_point(entry.path()))
            });
        for result in builder.build() {
            if is_cancelled() {
                return Err(ApiError::new(
                    "cancelled",
                    "The workspace search was cancelled.",
                ));
            }
            let Ok(entry) = result else { continue };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) || !is_markdown(entry.path()) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() > MAX_SEARCH_FILE_BYTES {
                continue;
            }
            let absolute = entry.path().to_string_lossy().into_owned();
            let relative = entry
                .path()
                .strip_prefix(&root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .into_owned();
            // Encoding must be settled before publishing any result: a valid
            // UTF-8 prefix can still belong to a legacy-encoded file. Decode
            // once, then stream callbacks while scanning the normalized text.
            let Ok(file) = fs::File::open(entry.path()) else {
                continue;
            };
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            let Ok(_) = file.take(MAX_SEARCH_FILE_BYTES + 1).read_to_end(&mut bytes) else {
                continue;
            };
            if bytes.len() as u64 > MAX_SEARCH_FILE_BYTES {
                continue;
            }
            let Ok(decoded) = encoding::decode(&bytes) else {
                continue;
            };
            for (index, line) in decoded.content.lines().enumerate() {
                if index % 256 == 0 && is_cancelled() {
                    return Err(ApiError::new(
                        "cancelled",
                        "The workspace search was cancelled.",
                    ));
                }
                let Some(hit) = search_line(
                    &absolute,
                    &relative,
                    index,
                    line,
                    &query,
                    request.case_sensitive,
                ) else {
                    continue;
                };
                on_hit(&hit)?;
                hit_count += 1;
                if hit_count >= limit {
                    return Ok(hit_count);
                }
            }
        }
        Ok(hit_count)
    }

    pub fn create_entry(
        &self,
        parent: &Path,
        name: &str,
        is_dir: bool,
    ) -> ApiResult<WorkspaceSnapshot> {
        self.create_entry_with_guard(parent, name, is_dir, |_| Ok(()))
    }

    #[cfg(feature = "cli")]
    pub fn create_entry_guarded<G, F>(
        &self,
        parent: &Path,
        name: &str,
        is_dir: bool,
        before_create: F,
    ) -> ApiResult<WorkspaceSnapshot>
    where
        F: FnOnce(&Path) -> ApiResult<G>,
    {
        self.create_entry_with_guard(parent, name, is_dir, before_create)
    }

    fn create_entry_with_guard<G, F>(
        &self,
        parent: &Path,
        name: &str,
        is_dir: bool,
        before_create: F,
    ) -> ApiResult<WorkspaceSnapshot>
    where
        F: FnOnce(&Path) -> ApiResult<G>,
    {
        let root = self.require_root()?;
        {
            let _path_lock = lock_path_mutations()?;
            let target = self.preview_create_entry(parent, name)?;
            let _directory_guard = before_create(&target)?;
            self.create_entry_at(&target, is_dir)?;
        }
        self.snapshot(&root)
    }

    fn create_entry_at(&self, target: &Path, is_dir: bool) -> ApiResult<()> {
        if is_dir {
            fs::create_dir(target)
                .map_err(|error| ApiError::io("Unable to create the folder", error))?;
        } else if let AtomicWriteOutcome::Conflict(_) = atomic_create_if_absent(target, b"")? {
            return Err(ApiError::new(
                "already_exists",
                "A file with this name was created by another process.",
            ));
        }
        Ok(())
    }

    pub fn preview_create_entry(&self, parent: &Path, name: &str) -> ApiResult<PathBuf> {
        validate_name(name)?;
        let root = self.require_root()?;
        let parent = ensure_within(&root, parent)?;
        if !parent.is_dir() {
            return Err(ApiError::new(
                "not_a_directory",
                "The workspace parent is not a directory.",
            ));
        }
        let target = ensure_within(&root, &parent.join(name))?;
        if target.exists() {
            return Err(ApiError::new(
                "already_exists",
                "A file with this name already exists.",
            ));
        }
        Ok(target)
    }

    #[cfg(any(feature = "cli", test))]
    pub fn rename_entry(&self, path: &Path, new_name: &str) -> ApiResult<WorkspaceSnapshot> {
        self.rename_entry_with(path, new_name, |_, _, _| {})
    }

    pub(crate) fn rename_entry_with<F>(
        &self,
        path: &Path,
        new_name: &str,
        after_rename: F,
    ) -> ApiResult<WorkspaceSnapshot>
    where
        F: FnOnce(&Path, &Path, bool),
    {
        self.rename_entry_with_guards(path, new_name, |_, _| Ok(()), after_rename)
    }

    #[cfg(feature = "cli")]
    pub fn rename_entry_guarded<G, F>(
        &self,
        path: &Path,
        new_name: &str,
        before_rename: F,
    ) -> ApiResult<WorkspaceSnapshot>
    where
        F: FnOnce(&Path, &Path) -> ApiResult<G>,
    {
        self.rename_entry_with_guards(path, new_name, before_rename, |_, _, _| {})
    }

    fn rename_entry_with_guards<G, B, A>(
        &self,
        path: &Path,
        new_name: &str,
        before_rename: B,
        after_rename: A,
    ) -> ApiResult<WorkspaceSnapshot>
    where
        B: FnOnce(&Path, &Path) -> ApiResult<G>,
        A: FnOnce(&Path, &Path, bool),
    {
        let root = self.require_root()?;
        {
            let _path_lock = lock_path_mutations()?;
            let (source, target) = self.preview_rename_entry(path, new_name)?;
            let _directory_guard = before_rename(&source, &target)?;
            let is_directory = source.is_dir();
            fs::rename(&source, &target)
                .map_err(|error| ApiError::io("Unable to rename the entry", error))?;
            after_rename(&source, &target, is_directory);
        }
        self.snapshot(&root)
    }

    pub fn preview_rename_entry(
        &self,
        path: &Path,
        new_name: &str,
    ) -> ApiResult<(PathBuf, PathBuf)> {
        validate_name(new_name)?;
        let root = self.require_root()?;
        let source = ensure_within(&root, path)?;
        if !source.exists() {
            return Err(ApiError::new(
                "not_found",
                "The workspace entry no longer exists.",
            ));
        }
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
        Ok((source, target))
    }

    pub fn trash_entry(&self, path: &Path) -> ApiResult<WorkspaceSnapshot> {
        self.trash_entry_with_guard(path, |_| Ok(()))
    }

    #[cfg(feature = "cli")]
    pub fn trash_entry_guarded<G, F>(
        &self,
        path: &Path,
        before_trash: F,
    ) -> ApiResult<WorkspaceSnapshot>
    where
        F: FnOnce(&Path) -> ApiResult<G>,
    {
        self.trash_entry_with_guard(path, before_trash)
    }

    fn trash_entry_with_guard<G, F>(
        &self,
        path: &Path,
        before_trash: F,
    ) -> ApiResult<WorkspaceSnapshot>
    where
        F: FnOnce(&Path) -> ApiResult<G>,
    {
        let root = self.require_root()?;
        {
            let _path_lock = lock_path_mutations()?;
            let target = self.preview_trash_entry(path)?;
            let _directory_guard = before_trash(&target)?;
            trash::delete(&target).map_err(|error| {
                ApiError::io("Unable to move the entry to the recycle bin", error)
            })?;
        }
        self.snapshot(&root)
    }

    pub fn preview_trash_entry(&self, path: &Path) -> ApiResult<PathBuf> {
        let root = self.require_root()?;
        let target = ensure_within(&root, path)?;
        if target == root {
            return Err(ApiError::new(
                "invalid_path",
                "The workspace root cannot be deleted.",
            ));
        }
        if !target.exists() {
            return Err(ApiError::new(
                "not_found",
                "The workspace entry no longer exists.",
            ));
        }
        Ok(target)
    }

    fn snapshot(&self, root: &Path) -> ApiResult<WorkspaceSnapshot> {
        let mut entries = Vec::new();
        let mut builder = WalkBuilder::new(root);
        let filter_root = root.to_path_buf();
        builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .follow_links(false)
            .sort_by_file_name(|left, right| left.cmp(right))
            .filter_entry(move |entry| {
                entry.path() == filter_root
                    || (!is_heavy_directory(entry.path()) && !is_reparse_point(entry.path()))
            });
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
    let byte_column = if case_sensitive {
        line.find(query)?
    } else {
        case_insensitive_match_offset(line, query)?
    };
    let column = line[..byte_column].chars().count() as u32 + 1;
    Some(SearchHit {
        path: path.to_string(),
        relative_path: relative_path.to_string(),
        line: index as u32 + 1,
        column,
        preview: line.trim().chars().take(240).collect(),
    })
}

fn case_insensitive_match_offset(line: &str, folded_query: &str) -> Option<usize> {
    let folded_line = line.to_lowercase();
    let folded_offset = folded_line.find(folded_query)?;
    let mut current_folded_offset = 0;
    for (source_offset, character) in line.char_indices() {
        for folded_character in character.to_lowercase() {
            if current_folded_offset == folded_offset {
                return Some(source_offset);
            }
            current_folded_offset += folded_character.len_utf8();
        }
    }
    None
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

fn is_reparse_point(path: &Path) -> bool {
    is_symbolic_link_or_junction(path).unwrap_or(true)
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
    use std::cell::Cell;

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
    fn case_insensitive_search_reports_columns_in_the_original_unicode_line() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("unicode.md"), "İx\n").unwrap();
        let store = WorkspaceStore::new();
        store.open(temp.path()).unwrap();

        let hits = store
            .search(SearchRequest {
                root: temp.path().to_string_lossy().into_owned(),
                query: "x".into(),
                case_sensitive: false,
                limit: None,
            })
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].column, 2);
    }

    #[test]
    fn search_waits_for_legacy_encoding_before_emitting_hits() {
        let temp = tempfile::tempdir().unwrap();
        // The first two bytes are valid UTF-8 for `é`, but the later 0xe9
        // forces the complete file through legacy encoding detection. Emitting
        // the first line before that decision would produce a false hit.
        fs::write(
            temp.path().join("legacy.md"),
            b"\xC3\xA9 provisional\n\xE9 actual\n",
        )
        .unwrap();
        let store = WorkspaceStore::new();
        store.open(temp.path()).unwrap();

        let hits = store
            .search(SearchRequest {
                root: temp.path().to_string_lossy().into_owned(),
                query: "é".into(),
                case_sensitive: true,
                limit: None,
            })
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
    }

    #[test]
    fn search_emits_a_hit_before_scanning_the_rest_of_the_decoded_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut content = String::from("InkFlow first\n");
        for _ in 0..512 {
            content.push_str("no match\n");
        }
        fs::write(temp.path().join("long.md"), content).unwrap();
        let store = WorkspaceStore::new();
        store.open(temp.path()).unwrap();
        let emitted = Cell::new(false);

        let result = store.search_with_control(
            SearchRequest {
                root: temp.path().to_string_lossy().into_owned(),
                query: "InkFlow".into(),
                case_sensitive: true,
                limit: None,
            },
            |_| {
                emitted.set(true);
                Ok(())
            },
            || emitted.get(),
        );

        assert!(emitted.get());
        assert_eq!(result.unwrap_err().code, "cancelled");
    }

    #[test]
    fn searches_utf16_markdown() {
        let temp = tempfile::tempdir().unwrap();
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "标题\nInkFlow UTF-16\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(temp.path().join("utf16.md"), bytes).unwrap();
        let store = WorkspaceStore::new();
        store.open(temp.path()).unwrap();

        let hits = store
            .search(SearchRequest {
                root: temp.path().to_string_lossy().into_owned(),
                query: "InkFlow".into(),
                case_sensitive: true,
                limit: None,
            })
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
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

    #[cfg(target_os = "windows")]
    #[test]
    fn workspace_rename_waits_for_the_shared_path_mutation_lock() {
        use std::{sync::mpsc, thread, time::Duration};

        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        let renamed = temp.path().join("renamed.md");
        fs::write(&document, "note").unwrap();
        let store = WorkspaceStore::new();
        store.open(temp.path()).unwrap();
        let first = lock_path_mutations().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            finished_tx
                .send(store.rename_entry(&document, "renamed.md"))
                .unwrap();
        });

        started_rx.recv().unwrap();
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "workspace rename escaped the shared path mutation lock"
        );
        drop(first);
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(renamed.is_file());
        worker.join().unwrap();
    }
}
