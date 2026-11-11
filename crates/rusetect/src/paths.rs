//! Where the repository's data lives (port of ml/paths.py plus the service's ROOT / MODELS).
//!
//! RUSETECT_ROOT  repository root (default: found by walking up from the working directory or the executable)
//! RUSETECT_DATA  folder with the raw datasets (default ROOT/data)
//! RUSETECT_WORK  folder for intermediate files (default ROOT/work)
use std::path::{Path, PathBuf};

fn is_root(p: &Path) -> bool {
    p.join("service").join("models").join("cascade_config.json").exists()
}

pub fn repo_root() -> PathBuf {
    if let Ok(r) = std::env::var("RUSETECT_ROOT") {
        return PathBuf::from(r);
    }
    let mut starts = vec![];
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        starts.push(exe);
    }
    for s in starts {
        let mut p: Option<&Path> = Some(s.as_path());
        while let Some(d) = p {
            if is_root(d) {
                return d.to_path_buf();
            }
            p = d.parent();
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn env_or(name: &str, default: PathBuf) -> PathBuf {
    std::env::var(format!("RUSETECT_{name}")).ok().filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or(default)
}

pub fn models_dir() -> PathBuf {
    repo_root().join("service").join("models")
}
pub fn static_dir() -> PathBuf {
    repo_root().join("service").join("static")
}
pub fn results_dir() -> PathBuf {
    repo_root().join("results")
}
pub fn data_dir() -> PathBuf {
    env_or("DATA", repo_root().join("data"))
}
pub fn work_dir() -> PathBuf {
    let w = env_or("WORK", repo_root().join("work"));
    let _ = std::fs::create_dir_all(&w);
    w
}
pub fn export_dir() -> PathBuf {
    let e = repo_root().join("ml").join("exported");
    let _ = std::fs::create_dir_all(&e);
    e
}
