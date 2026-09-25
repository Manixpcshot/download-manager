use anyhow::Result;
use std::path::Path;

pub fn set_start_with_windows(enabled: bool) -> Result<()> {
    #[cfg(windows)]
    {
        use anyhow::Context;
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            winreg::enums::KEY_SET_VALUE,
        )?;
        let value_name = "PulseDownloadManager";
        if enabled {
            let executable = std::env::current_exe().context("cannot locate the application executable")?;
            let command = format!("\"{}\"", executable.display());
            key.set_value(value_name, &command)?;
        } else {
            let _ = key.delete_value(value_name);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        anyhow::bail!("Start with Windows is only available on Windows")
    }
}

pub fn open_file(path: &Path) -> Result<()> {
    open::that(path)?;
    Ok(())
}

pub fn open_folder(path: &Path) -> Result<()> {
    let folder = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    open::that(folder)?;
    Ok(())
}
