//! Project workspace: creation, file tree, editing, search, backups, versions.

use crate::error::{Result, TerError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

pub mod templates;

const MAX_TREE_ENTRIES: usize = 4_000;
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub template: String,
    pub path: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub rojo: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProjectIndex {
    #[serde(default)]
    projects: Vec<Project>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileNode {
    pub name: String,
    /// Path relative to the project root, using `/` separators.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<FileNode>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub path: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub id: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub bytes: u64,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub id: String,
    pub path: String,
    pub at: DateTime<Utc>,
    pub bytes: u64,
    pub snapshot: PathBuf,
}

pub struct ProjectStore {
    root: PathBuf,
    index_path: PathBuf,
    projects: Mutex<Vec<Project>>,
}

impl ProjectStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| TerError::io(root.display().to_string(), e))?;
        let index_path = root.join("projects.json");
        let projects = match std::fs::read_to_string(&index_path) {
            Ok(raw) => serde_json::from_str::<ProjectIndex>(&raw)
                .map(|i| i.projects)
                .unwrap_or_else(|e| {
                    tracing::warn!("projects.json unreadable ({e}); starting a fresh index");
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        };
        // Drop entries whose folder was removed outside the app.
        let projects: Vec<Project> = projects
            .into_iter()
            .filter(|p| p.path.is_dir())
            .collect();
        let store = Self {
            root,
            index_path,
            projects: Mutex::new(projects),
        };
        store.persist()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn persist(&self) -> Result<()> {
        let projects = self.projects.lock().expect("project lock").clone();
        let index = ProjectIndex { projects };
        let json = serde_json::to_string_pretty(&index)
            .map_err(|e| TerError::json("projects.json", e))?;
        let tmp = self.index_path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| TerError::io(tmp.display().to_string(), e))?;
        std::fs::rename(&tmp, &self.index_path)
            .map_err(|e| TerError::io(self.index_path.display().to_string(), e))?;
        Ok(())
    }

    pub fn list(&self) -> Vec<Project> {
        let mut projects = self.projects.lock().expect("project lock").clone();
        projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        projects
    }

    pub fn get(&self, id: &str) -> Option<Project> {
        self.projects
            .lock()
            .expect("project lock")
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    pub fn create(&self, name: &str, template: &str, description: &str) -> Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TerError::Invalid("Project name cannot be empty".into()));
        }
        let slug = slugify(name);
        let id = uuid::Uuid::new_v4().to_string();
        let short: String = id.chars().take(8).collect();
        let path = self.root.join(format!("{slug}-{short}"));
        std::fs::create_dir_all(&path).map_err(|e| TerError::io(path.display().to_string(), e))?;

        let template = templates::find(template).unwrap_or_else(|| templates::blank());
        for (rel, contents) in template.files {
            let target = safe_join(&path, rel)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| TerError::io(parent.display().to_string(), e))?;
            }
            std::fs::write(&target, contents)
                .map_err(|e| TerError::io(target.display().to_string(), e))?;
        }

        let now = Utc::now();
        let project = Project {
            id,
            name: name.to_string(),
            description: description.trim().to_string(),
            template: template.id.to_string(),
            path,
            created_at: now,
            updated_at: now,
            rojo: true,
        };
        self.projects
            .lock()
            .expect("project lock")
            .push(project.clone());
        self.persist()?;
        Ok(project)
    }

    /// Adds an existing folder (for example a Rojo checkout) to the workspace.
    pub fn import(&self, path: &Path, name: Option<&str>) -> Result<Project> {
        if !path.is_dir() {
            return Err(TerError::Invalid(format!(
                "{} is not a folder",
                path.display()
            )));
        }
        let display_name = name
            .map(|n| n.to_string())
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "Imported project".to_string());
        let now = Utc::now();
        let project = Project {
            id: uuid::Uuid::new_v4().to_string(),
            name: display_name,
            description: String::new(),
            template: "imported".into(),
            path: path.to_path_buf(),
            created_at: now,
            updated_at: now,
            rojo: path.join("default.project.json").is_file()
                || path.join("project.json").is_file(),
        };
        self.projects
            .lock()
            .expect("project lock")
            .push(project.clone());
        self.persist()?;
        Ok(project)
    }

    pub fn remove(&self, id: &str, delete_files: bool) -> Result<()> {
        let project = self
            .get(id)
            .ok_or_else(|| TerError::ProjectNotFound(id.to_string()))?;
        self.projects
            .lock()
            .expect("project lock")
            .retain(|p| p.id != id);
        self.persist()?;
        if delete_files {
            // Move to a trash folder instead of destroying user work.
            let trash = self.root.join(".trash");
            std::fs::create_dir_all(&trash)
                .map_err(|e| TerError::io(trash.display().to_string(), e))?;
            let dest = trash.join(format!(
                "{}-{}",
                project
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "project".into()),
                Utc::now().format("%Y%m%d%H%M%S")
            ));
            std::fs::rename(&project.path, &dest)
                .map_err(|e| TerError::io(project.path.display().to_string(), e))?;
        }
        Ok(())
    }

    pub fn touch(&self, id: &str) -> Result<()> {
        let mut projects = self.projects.lock().expect("project lock");
        if let Some(project) = projects.iter_mut().find(|p| p.id == id) {
            project.updated_at = Utc::now();
        }
        drop(projects);
        self.persist()
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<Project> {
        let mut projects = self.projects.lock().expect("project lock");
        let project = projects
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| TerError::ProjectNotFound(id.to_string()))?;
        project.name = name.trim().to_string();
        project.updated_at = Utc::now();
        let cloned = project.clone();
        drop(projects);
        self.persist()?;
        Ok(cloned)
    }

    pub fn project_path(&self, id: &str) -> Result<PathBuf> {
        self.get(id)
            .map(|p| p.path)
            .ok_or_else(|| TerError::ProjectNotFound(id.to_string()))
    }

    pub fn tree(&self, id: &str) -> Result<Vec<FileNode>> {
        let root = self.project_path(id)?;
        let mut count = 0;
        Ok(read_tree(&root, &root, 0, &mut count))
    }

    pub fn read_file(&self, id: &str, rel: &str) -> Result<String> {
        let root = self.project_path(id)?;
        let path = safe_join(&root, rel)?;
        let meta = std::fs::metadata(&path).map_err(|e| TerError::io(path.display().to_string(), e))?;
        if meta.len() as usize > MAX_FILE_BYTES {
            return Err(TerError::Invalid(format!(
                "{} is larger than the 4 MB editor limit",
                rel
            )));
        }
        std::fs::read_to_string(&path).map_err(|e| TerError::io(path.display().to_string(), e))
    }

    /// Writes a file, snapshotting the previous contents into version history.
    pub fn write_file(&self, id: &str, rel: &str, contents: &str) -> Result<()> {
        let root = self.project_path(id)?;
        let path = safe_join(&root, rel)?;
        if path.exists() {
            self.snapshot(&root, rel, &path)?;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| TerError::io(parent.display().to_string(), e))?;
        }
        std::fs::write(&path, contents).map_err(|e| TerError::io(path.display().to_string(), e))?;
        self.touch(id)?;
        Ok(())
    }

    pub fn create_file(&self, id: &str, rel: &str, contents: &str) -> Result<()> {
        let root = self.project_path(id)?;
        let path = safe_join(&root, rel)?;
        if path.exists() {
            return Err(TerError::Invalid(format!("{rel} already exists")));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| TerError::io(parent.display().to_string(), e))?;
        }
        std::fs::write(&path, contents).map_err(|e| TerError::io(path.display().to_string(), e))?;
        self.touch(id)?;
        Ok(())
    }

    pub fn delete_path(&self, id: &str, rel: &str) -> Result<()> {
        let root = self.project_path(id)?;
        let path = safe_join(&root, rel)?;
        if path == root {
            return Err(TerError::Invalid("refusing to delete the project root".into()));
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
                .map_err(|e| TerError::io(path.display().to_string(), e))?;
        } else {
            std::fs::remove_file(&path)
                .map_err(|e| TerError::io(path.display().to_string(), e))?;
        }
        self.touch(id)?;
        Ok(())
    }

    pub fn rename_path(&self, id: &str, from: &str, to: &str) -> Result<()> {
        let root = self.project_path(id)?;
        let source = safe_join(&root, from)?;
        let target = safe_join(&root, to)?;
        if target.exists() {
            return Err(TerError::Invalid(format!("{to} already exists")));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| TerError::io(parent.display().to_string(), e))?;
        }
        std::fs::rename(&source, &target)
            .map_err(|e| TerError::io(source.display().to_string(), e))?;
        self.touch(id)?;
        Ok(())
    }

    pub fn search(&self, id: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let root = self.project_path(id)?;
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        let mut stack = vec![root.clone()];
        let mut visited = 0;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "node_modules" {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                visited += 1;
                if visited > MAX_TREE_ENTRIES || hits.len() >= limit {
                    return Ok(hits);
                }
                if !is_text_file(&path) {
                    continue;
                }
                let Ok(contents) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let rel = relative(&root, &path);
                for (n, line) in contents.lines().enumerate() {
                    if line.to_lowercase().contains(&needle) {
                        hits.push(SearchHit {
                            path: rel.clone(),
                            line: (n + 1) as u32,
                            text: line.chars().take(200).collect(),
                        });
                        if hits.len() >= limit {
                            return Ok(hits);
                        }
                    }
                }
            }
        }
        Ok(hits)
    }

    fn versions_dir(root: &Path) -> PathBuf {
        root.join(".tercode").join("versions")
    }

    fn snapshot(&self, root: &Path, rel: &str, path: &Path) -> Result<()> {
        let dir = Self::versions_dir(root);
        std::fs::create_dir_all(&dir).map_err(|e| TerError::io(dir.display().to_string(), e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let stamp = Utc::now().format("%Y%m%dT%H%M%S%.3f");
        let snapshot = dir.join(format!("{stamp}-{}.snap", &id[..8]));
        std::fs::copy(path, &snapshot)
            .map_err(|e| TerError::io(snapshot.display().to_string(), e))?;
        let entry = VersionEntry {
            id,
            path: rel.to_string(),
            at: Utc::now(),
            bytes: std::fs::metadata(&snapshot).map(|m| m.len()).unwrap_or(0),
            snapshot,
        };
        let log_path = dir.join("versions.jsonl");
        let line = serde_json::to_string(&entry).map_err(|e| TerError::json("version entry", e))?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| TerError::io(log_path.display().to_string(), e))?;
        writeln!(file, "{line}").map_err(|e| TerError::io(log_path.display().to_string(), e))?;
        Ok(())
    }

    pub fn versions(&self, id: &str) -> Result<Vec<VersionEntry>> {
        let root = self.project_path(id)?;
        let log_path = Self::versions_dir(&root).join("versions.jsonl");
        let Ok(raw) = std::fs::read_to_string(&log_path) else {
            return Ok(Vec::new());
        };
        let mut entries: Vec<VersionEntry> = raw
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        entries.reverse();
        entries.truncate(200);
        Ok(entries)
    }

    pub fn restore_version(&self, id: &str, version_id: &str) -> Result<String> {
        let entries = self.versions(id)?;
        let entry = entries
            .into_iter()
            .find(|e| e.id == version_id)
            .ok_or_else(|| TerError::Invalid("version snapshot not found".into()))?;
        let contents = std::fs::read_to_string(&entry.snapshot)
            .map_err(|e| TerError::io(entry.snapshot.display().to_string(), e))?;
        self.write_file(id, &entry.path, &contents)?;
        Ok(entry.path)
    }

    pub fn backup(&self, id: &str, label: &str) -> Result<BackupInfo> {
        let project = self
            .get(id)
            .ok_or_else(|| TerError::ProjectNotFound(id.to_string()))?;
        let dir = self.root.join("backups").join(&project.id);
        std::fs::create_dir_all(&dir).map_err(|e| TerError::io(dir.display().to_string(), e))?;
        let stamp = Utc::now().format("%Y%m%dT%H%M%S");
        let archive = dir.join(format!("backup-{stamp}.json"));
        let files = collect_files(&project.path)?;
        let json = serde_json::to_string_pretty(&files)
            .map_err(|e| TerError::json("backup", e))?;
        std::fs::write(&archive, &json)
            .map_err(|e| TerError::io(archive.display().to_string(), e))?;
        let info = BackupInfo {
            id: format!("{stamp}"),
            label: label.trim().to_string(),
            created_at: Utc::now(),
            bytes: json.len() as u64,
            path: archive,
        };
        Ok(info)
    }

    pub fn backups(&self, id: &str) -> Result<Vec<BackupInfo>> {
        let dir = self.root.join("backups").join(id);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out: Vec<BackupInfo> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_string_lossy().to_string();
                let stamp = name.trim_start_matches("backup-").trim_end_matches(".json");
                Some(BackupInfo {
                    id: stamp.to_string(),
                    label: String::new(),
                    created_at: chrono::DateTime::parse_from_rfc3339(&format!(
                        "{}-01T00:00:00Z",
                        &stamp[..stamp.len().min(8)]
                    ))
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now),
                    bytes: entry.metadata().map(|m| m.len()).unwrap_or(0),
                    path,
                })
            })
            .collect();
        out.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(out)
    }

    pub fn restore_backup(&self, id: &str, backup_id: &str) -> Result<usize> {
        let project = self
            .get(id)
            .ok_or_else(|| TerError::ProjectNotFound(id.to_string()))?;
        let path = self
            .root
            .join("backups")
            .join(&project.id)
            .join(format!("backup-{backup_id}.json"));
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| TerError::io(path.display().to_string(), e))?;
        let files: Vec<BackupFile> = serde_json::from_str(&raw)
            .map_err(|e| TerError::json("backup file", e))?;
        let mut restored = 0;
        for file in files {
            let target = safe_join(&project.path, &file.path)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| TerError::io(parent.display().to_string(), e))?;
            }
            std::fs::write(&target, file.contents)
                .map_err(|e| TerError::io(target.display().to_string(), e))?;
            restored += 1;
        }
        self.touch(id)?;
        Ok(restored)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupFile {
    path: String,
    contents: String,
}

fn collect_files(root: &Path) -> Result<Vec<BackupFile>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| TerError::io(dir.display().to_string(), e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".tercode" || name == ".git" || name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if is_text_file(&path) {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    out.push(BackupFile {
                        path: relative(root, &path),
                        contents,
                    });
                }
            }
        }
    }
    Ok(out)
}

fn is_text_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        "lua" | "luau" | "json" | "toml" | "md" | "txt" | "rbxmx" | "rbxlx" | "yaml" | "yml"
    )
}

fn read_tree(root: &Path, dir: &Path, depth: usize, count: &mut usize) -> Vec<FileNode> {
    if depth > 8 || *count > MAX_TREE_ENTRIES {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut nodes: Vec<FileNode> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" {
                return None;
            }
            let path = entry.path();
            *count += 1;
            let meta = entry.metadata().ok()?;
            if path.is_dir() {
                Some(FileNode {
                    name,
                    path: relative(root, &path),
                    is_dir: true,
                    size: 0,
                    children: Some(read_tree(root, &path, depth + 1, count)),
                })
            } else {
                Some(FileNode {
                    name,
                    path: relative(root, &path),
                    is_dir: false,
                    size: meta.len(),
                    children: None,
                })
            }
        })
        .collect();
    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    nodes
}

pub fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// Joins a user-supplied relative path, rejecting anything that escapes the
/// project root. This is the single choke point for all file operations.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim().replace('\\', "/");
    if rel.is_empty() {
        return Err(TerError::InvalidPath("empty path".into()));
    }
    if rel.contains('\0') {
        return Err(TerError::InvalidPath(rel));
    }
    let candidate = Path::new(&rel);
    for component in candidate.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(TerError::InvalidPath(format!(
                    "path must stay inside the project: {rel}"
                )))
            }
        }
    }
    Ok(root.join(candidate))
}

fn slugify(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    let mut out = String::new();
    let mut last_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !last_dash {
                out.push(c);
            }
            last_dash = true;
        } else {
            out.push(c);
            last_dash = false;
        }
    }
    if out.is_empty() {
        "project".into()
    } else {
        out.chars().take(48).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, ProjectStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn creates_project_with_template_files() {
        let (_dir, store) = store();
        let project = store.create("My Game", "obby", "").unwrap();
        assert!(project.path.is_dir());
        assert!(project.path.join("default.project.json").is_file());
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn rejects_empty_name() {
        let (_dir, store) = store();
        assert!(store.create("   ", "blank", "").is_err());
    }

    #[test]
    fn path_traversal_is_blocked() {
        let (_dir, store) = store();
        let project = store.create("Safe", "blank", "").unwrap();
        for bad in ["../escape", "a/../../b", "/etc/passwd", r"..\windows\system32"] {
            assert!(
                store.read_file(&project.id, bad).is_err(),
                "expected rejection for {bad}"
            );
        }
        let ok = store.write_file(&project.id, "src/main.luau", "print('hi')");
        assert!(ok.is_ok());
        assert_eq!(
            store.read_file(&project.id, "src/main.luau").unwrap(),
            "print('hi')"
        );
    }

    #[test]
    fn write_snapshots_previous_version() {
        let (_dir, store) = store();
        let project = store.create("Versions", "blank", "").unwrap();
        store.write_file(&project.id, "src/a.luau", "v1").unwrap();
        store.write_file(&project.id, "src/a.luau", "v2").unwrap();
        let versions = store.versions(&project.id).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].path, "src/a.luau");
        store.restore_version(&project.id, &versions[0].id).unwrap();
        assert_eq!(store.read_file(&project.id, "src/a.luau").unwrap(), "v1");
    }

    #[test]
    fn tree_lists_files_and_folders() {
        let (_dir, store) = store();
        let project = store.create("Tree", "blank", "").unwrap();
        store.write_file(&project.id, "src/server/Main.server.luau", "print(1)").unwrap();
        let tree = store.tree(&project.id).unwrap();
        let src = tree.iter().find(|n| n.name == "src").unwrap();
        assert!(src.is_dir);
        let children = src.children.as_ref().unwrap();
        assert!(children.iter().any(|c| c.name == "server"));
    }

    #[test]
    fn search_finds_lines() {
        let (_dir, store) = store();
        let project = store.create("Search", "blank", "").unwrap();
        store
            .write_file(
                &project.id,
                "src/x.luau",
                "local a = 1\nlocal DataStoreService = 2\n",
            )
            .unwrap();
        let hits = store.search(&project.id, "datastore", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].path, "src/x.luau");
    }

    #[test]
    fn backup_and_restore() {
        let (_dir, store) = store();
        let project = store.create("Backup", "blank", "").unwrap();
        store.write_file(&project.id, "src/a.luau", "original").unwrap();
        let info = store.backup(&project.id, "before refactor").unwrap();
        store.write_file(&project.id, "src/a.luau", "changed").unwrap();
        let restored = store.restore_backup(&project.id, &info.id).unwrap();
        assert!(restored >= 1);
        assert_eq!(store.read_file(&project.id, "src/a.luau").unwrap(), "original");
    }

    #[test]
    fn delete_moves_to_trash_not_destroy() {
        let (dir, store) = store();
        let project = store.create("Trash", "blank", "").unwrap();
        let path = project.path.clone();
        store.remove(&project.id, true).unwrap();
        assert!(!path.exists());
        let trash = dir.path().join(".trash");
        assert!(trash.is_dir());
        assert_eq!(std::fs::read_dir(trash).unwrap().count(), 1);
    }

    #[test]
    fn index_survives_restart() {
        let (dir, store) = store();
        store.create("Persisted", "blank", "").unwrap();
        drop(store);
        let reopened = ProjectStore::open(dir.path()).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.list()[0].name, "Persisted");
    }

    #[test]
    fn rename_and_import() {
        let (dir, store) = store();
        let project = store.create("Rename Me", "blank", "").unwrap();
        let renamed = store.rename(&project.id, "Renamed").unwrap();
        assert_eq!(renamed.name, "Renamed");

        let external = dir.path().join("external");
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("default.project.json"), "{}").unwrap();
        let imported = store.import(&external, None).unwrap();
        assert!(imported.rojo);
        assert_eq!(imported.name, "external");
    }

    #[test]
    fn slugify_is_filesystem_safe() {
        assert_eq!(slugify("My Cool Game!"), "my-cool-game");
        assert_eq!(slugify("///"), "project");
        assert!(slugify(&"a".repeat(100)).len() <= 48);
    }
}
