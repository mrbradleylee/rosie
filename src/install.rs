use crate::paths::{config_dir, local_bin_dir, local_man_dir, manpath_contains, path_contains};
use anyhow::{Result, anyhow};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub fn install(man_page: &str) -> Result<()> {
    let source = env::current_exe()?;
    let bin_dir = local_bin_dir()?;
    fs::create_dir_all(&bin_dir)?;

    let file_name = source
        .file_name()
        .ok_or_else(|| anyhow!("Unable to determine executable name"))?;
    let destination = bin_dir.join(file_name);

    if source == destination {
        println!("Rosie is already installed at {}", destination.display());
        return Ok(());
    }

    fs::copy(&source, &destination)?;
    set_executable_permissions(&destination)?;

    println!("Installed Rosie to {}", destination.display());

    let man_page_path = install_man_page(man_page)?;
    println!("Installed man page to {}", man_page_path.display());
    let themes_dir = install_packaged_themes()?;
    println!("Installed bundled themes to {}", themes_dir.display());

    if !path_contains(&bin_dir) {
        println!(
            "{} is not on your PATH. Add it to run `rosie` directly.",
            bin_dir.display()
        );
    }

    let man_root = man_page_path
        .ancestors()
        .nth(2)
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| man_page_path.clone());
    if !manpath_contains(&man_root) {
        println!(
            "{} is not on your MANPATH. You may need to add it to use `man rosie` directly.",
            man_root.display()
        );
    }

    Ok(())
}

fn install_man_page(man_page: &str) -> Result<PathBuf> {
    let man_dir = local_man_dir()?;
    fs::create_dir_all(&man_dir)?;
    let man_page_path = man_dir.join("rosie.1");
    fs::write(&man_page_path, man_page)?;
    Ok(man_page_path)
}

fn install_packaged_themes() -> Result<PathBuf> {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes");
    let destination_dir = config_dir()?.join("themes");
    fs::create_dir_all(&destination_dir)?;

    let entries = fs::read_dir(&source_dir).map_err(|err| {
        anyhow!(
            "Failed to read bundled themes '{}': {err}",
            source_dir.display()
        )
    })?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow!("Theme file missing name: {}", path.display()))?;
        fs::copy(&path, destination_dir.join(file_name)).map_err(|err| {
            anyhow!(
                "Failed to install bundled theme '{}' to '{}': {err}",
                path.display(),
                destination_dir.display()
            )
        })?;
    }

    Ok(destination_dir)
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
