use super::library::{Job, Kind, Status, video_duration_ms};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tokio::{fs, sync::Mutex};

pub static MUTATION: Mutex<()> = Mutex::const_new(());
const FILE: &str = "restored-trims.json";
type Restored = HashMap<i64, HashSet<String>>;
static DURATIONS: LazyLock<Mutex<HashMap<PathBuf, i64>>> = LazyLock::new(Mutex::default);

pub async fn duration_ms(path: PathBuf) -> Option<i64> {
    if let Some(duration) = DURATIONS.lock().await.get(&path).copied() {
        return Some(duration);
    }
    let duration = video_duration_ms(&path).await.ok()?;
    DURATIONS.lock().await.insert(path, duration);
    Some(duration)
}

async fn load(root: &Path) -> io::Result<Restored> {
    match fs::read(root.join(FILE)).await {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(error),
    }
}

pub async fn current<'a>(root: &Path, jobs: &'a [Job]) -> io::Result<HashMap<i64, &'a Job>> {
    let restored = load(root).await?;
    let mut current = HashMap::new();
    for job in jobs {
        if let (Kind::Trim, Status::Ready, [id]) = (job.kind, job.status, job.clip_ids.as_slice())
            && !restored
                .get(id)
                .is_some_and(|ignored| ignored.contains(&job.id))
        {
            current.entry(*id).or_insert(job);
        }
    }
    Ok(current)
}

pub async fn restore(root: &Path, id: i64, jobs: &[Job]) -> io::Result<()> {
    let mut restored = load(root).await?;
    let ignored = restored.entry(id).or_default();
    ignored.extend(
        jobs.iter()
            .filter(|job| job.kind == Kind::Trim && job.clip_ids == [id])
            .map(|job| job.id.clone()),
    );
    save(root, &restored).await
}

pub async fn forget(root: &Path, id: i64) -> io::Result<()> {
    let mut restored = load(root).await?;
    if restored.remove(&id).is_some() {
        save(root, &restored).await?;
    }
    Ok(())
}

async fn save(root: &Path, restored: &Restored) -> io::Result<()> {
    let temporary = tempfile::NamedTempFile::new_in(root)?;
    fs::write(temporary.path(), serde_json::to_vec(restored)?).await?;
    temporary
        .persist(root.join(FILE))
        .map_err(|error| error.error)?;
    Ok(())
}
