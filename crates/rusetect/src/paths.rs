//! Where the repository's data lives (the service's ROOT / MODELS).
//!
//! RUSETECT_ROOT  repository root (default: found by walking up from the working directory or the executable)
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

pub fn models_dir() -> PathBuf {
    repo_root().join("service").join("models")
}
pub fn static_dir() -> PathBuf {
    repo_root().join("service").join("static")
}
