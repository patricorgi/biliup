use super::library::{ffmpeg, video_duration_ms};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use tokio::{fs, sync::Mutex};

const SUPPORTED: &[&str] = &["flv", "mp4", "m4v", "mov", "mkv", "ts", "webm"];
const MAX_PER_IMPORT: usize = 200;
pub(crate) static WRITING: Mutex<()> = Mutex::const_new(());
pub(crate) static REMUX: Mutex<()> = Mutex::const_new(());

#[derive(Clone, Serialize, Deserialize)]
pub struct ImportedClip {
    pub id: i64,
    relative_path: PathBuf,
    pub duration_ms: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    deleted: bool,
}

#[derive(Serialize)]
pub struct ImportSummary {
    pub added: usize,
    pub duplicates: usize,
    pub errors: Vec<String>,
}

pub struct Imports {
    pub items: Vec<ImportedClip>,
    pub home: PathBuf,
}

impl Imports {
    pub async fn load(root: &Path) -> io::Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| io::Error::other("无法找到主文件夹"))?;
        let home = fs::canonicalize(home).await?;
        let items = match fs::read(root.join("imports.json")).await {
            Ok(data) => serde_json::from_slice(&data)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        Ok(Self { items, home })
    }

    pub async fn path(&self, item: &ImportedClip) -> Option<PathBuf> {
        if item.deleted {
            return None;
        }
        let path = fs::canonicalize(self.home.join(&item.relative_path))
            .await
            .ok()?;
        (path.starts_with(&self.home)
            && supported(&path)
            && fs::metadata(&path).await.ok()?.is_file())
        .then_some(path)
    }

    pub async fn trash_path(&self, id: i64) -> io::Result<PathBuf> {
        let item = self
            .items
            .iter()
            .find(|item| item.id == id && !item.deleted)
            .ok_or(io::ErrorKind::NotFound)?;
        let path = self.path(item).await.ok_or(io::ErrorKind::NotFound)?;
        if path != self.home.join(&item.relative_path) {
            return Err(io::Error::other("导入文件的路径已变化"));
        }
        Ok(path)
    }

    /// Keep allocated IDs reserved so older joins never point to a newly imported clip.
    pub async fn mark_deleted(&mut self, root: &Path, id: i64) -> io::Result<()> {
        let mut items = self.items.clone();
        let item = items
            .iter_mut()
            .find(|item| item.id == id && !item.deleted)
            .ok_or(io::ErrorKind::NotFound)?;
        item.deleted = true;
        save(root, &items).await?;
        self.items = items;
        Ok(())
    }
}

impl ImportedClip {
    pub fn filename(&self) -> String {
        self.relative_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    pub fn display_path(&self) -> String {
        format!(
            "~/{}",
            self.relative_path
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        )
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

async fn save(root: &Path, items: &[ImportedClip]) -> io::Result<()> {
    fs::create_dir_all(root).await?;
    let part = tempfile::NamedTempFile::new_in(root)?;
    fs::write(part.path(), serde_json::to_vec(items)?).await?;
    part.persist(root.join("imports.json"))
        .map_err(|error| error.error)?;
    Ok(())
}

fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            SUPPORTED
                .iter()
                .any(|supported| ext.eq_ignore_ascii_case(supported))
        })
}

fn user_path(input: &str, home: &Path) -> Option<PathBuf> {
    if let Some(relative) = input.strip_prefix("~/") {
        Some(home.join(relative))
    } else if Path::new(input).is_absolute() {
        Some(PathBuf::from(input))
    } else {
        None
    }
}

pub async fn add(root: &Path, paths: &[String]) -> io::Result<ImportSummary> {
    let _lock = WRITING.lock().await;
    let mut imports = Imports::load(root).await?;
    let mut summary = ImportSummary {
        added: 0,
        duplicates: 0,
        errors: Vec::new(),
    };
    let mut candidates = BTreeSet::new();
    for input in paths {
        let Some(path) = user_path(input.trim(), &imports.home) else {
            summary
                .errors
                .push(format!("{input}：请输入完整路径或 ~/ 开头的路径"));
            continue;
        };
        let Ok(path) = fs::canonicalize(&path).await else {
            summary.errors.push(format!("{input}：找不到文件或文件夹"));
            continue;
        };
        if !path.starts_with(&imports.home) {
            summary
                .errors
                .push(format!("{input}：请选取主文件夹内的文件"));
            continue;
        }
        let meta = fs::metadata(&path).await?;
        if meta.is_dir() {
            let mut entries = fs::read_dir(&path).await?;
            while let Some(entry) = entries.next_entry().await? {
                if entry.file_type().await?.is_file() && supported(&entry.path()) {
                    candidates.insert(entry.path());
                    if candidates.len() > MAX_PER_IMPORT {
                        break;
                    }
                }
            }
        } else if meta.is_file() && supported(&path) {
            candidates.insert(path);
        } else {
            summary.errors.push(format!("{input}：不支持此文件格式"));
        }
        if candidates.len() > MAX_PER_IMPORT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "单次最多导入 200 个视频，请分批导入",
            ));
        }
    }
    let mut known: BTreeSet<_> = imports
        .items
        .iter()
        .filter(|item| !item.deleted)
        .map(|item| item.relative_path.clone())
        .collect();
    let mut id = imports.items.iter().map(|item| item.id).min().unwrap_or(0) - 1;
    for path in candidates {
        let relative = path.strip_prefix(&imports.home).unwrap().to_path_buf();
        if !known.insert(relative.clone()) {
            summary.duplicates += 1;
            continue;
        }
        match video_duration_ms(&path).await {
            Ok(duration_ms) => {
                imports.items.push(ImportedClip {
                    id,
                    relative_path: relative,
                    duration_ms,
                    deleted: false,
                });
                id -= 1;
                summary.added += 1;
            }
            Err(error) => summary
                .errors
                .push(format!("~/{}：{error}", relative.display())),
        }
    }
    if summary.added != 0 {
        save(root, &imports.items).await?;
    }
    Ok(summary)
}

pub async fn preview(root: &Path, input: &Path, key: &str) -> Result<PathBuf, String> {
    if input
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
    {
        return Ok(input.to_owned());
    }
    let folder = root.join(".previews");
    fs::create_dir_all(&folder)
        .await
        .map_err(|error| error.to_string())?;
    let output = folder.join(format!("{key}.mp4"));
    let modified = fs::metadata(input)
        .await
        .and_then(|meta| meta.modified())
        .map_err(|error| error.to_string())?;
    if cached_preview(&output, modified).await {
        return Ok(output);
    }
    let _lock = REMUX.lock().await;
    if cached_preview(&output, modified).await {
        return Ok(output);
    }
    let scratch = tempfile::tempdir_in(&folder).map_err(|error| error.to_string())?;
    let part = scratch.path().join("preview.mp4");
    ffmpeg(
        crate::tools::ffmpeg_command()
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-protocol_whitelist",
                "file",
                "-format_whitelist",
                "flv,mov,matroska,mpegts",
                "-i",
            ])
            .arg(input)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0?",
                "-c",
                "copy",
                "-movflags",
                "+faststart",
                "-y",
            ])
            .arg(&part),
    )
    .await?;
    fs::rename(part, &output)
        .await
        .map_err(|error| error.to_string())?;
    Ok(output)
}

async fn cached_preview(path: &Path, modified: std::time::SystemTime) -> bool {
    fs::metadata(path)
        .await
        .and_then(|meta| meta.modified())
        .is_ok_and(|cached| cached >= modified)
}
