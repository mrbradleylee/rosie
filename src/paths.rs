use anyhow::{Result, anyhow};
use std::env;
use std::path::{Path, PathBuf};

pub fn config_path() -> Result<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| anyhow!("Unable to determine config directory"))?;
    Ok(base.join("rosie").join("config.toml"))
}

pub fn config_dir() -> Result<PathBuf> {
    config_path()?
        .parent()
        .map(|path| path.to_path_buf())
        .ok_or_else(|| anyhow!("Unable to determine config directory"))
}

pub fn app_data_dir() -> Result<PathBuf> {
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| anyhow!("Unable to determine data directory"))?;
    Ok(base.join("rosie"))
}

pub fn local_bin_dir() -> Result<PathBuf> {
    env::var_os("XDG_BIN_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/bin")))
        .ok_or_else(|| anyhow!("Unable to determine local bin directory"))
}

pub fn local_man_dir() -> Result<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|path| path.join("man").join("man1"))
        .ok_or_else(|| anyhow!("Unable to determine local man directory"))
}

pub fn path_contains(dir: &Path) -> bool {
    env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|entry| entry == dir))
        .unwrap_or(false)
}

pub fn manpath_contains(dir: &Path) -> bool {
    env::var_os("MANPATH")
        .map(|path| env::split_paths(&path).any(|entry| entry == dir))
        .unwrap_or(false)
}
