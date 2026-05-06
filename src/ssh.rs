use color_eyre::eyre::{Result, bail};
use std::process::Command;

use crate::store::SshKey;

pub const SHELLS: &[&str] = &[
    "/bin/bash",
    "/usr/bin/rbash",
    "/usr/sbin/nologin",
    "/bin/sh",
];

/// Crea el usuario del sistema con home y shell indicados.
/// Bloquea password para forzar autenticación solo por clave SSH.
pub fn create_user(username: &str, shell: &str, home_dir: &str) -> Result<()> {
    let output = Command::new("useradd")
        .args(["-m", "-s", shell, "-d", home_dir, username])
        .output()?;
    if !output.status.success() {
        bail!(
            "useradd: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // Bloquear password → solo SSH key
    let _ = Command::new("passwd").args(["-l", username]).output();
    Ok(())
}

/// Elimina el usuario del sistema (sin borrar home).
pub fn delete_user(username: &str) -> Result<()> {
    let output = Command::new("userdel").arg(username).output()?;
    if !output.status.success() {
        bail!(
            "userdel: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Cambia la shell del usuario en el sistema.
pub fn set_shell(username: &str, shell: &str) -> Result<()> {
    let output = Command::new("usermod")
        .args(["-s", shell, username])
        .output()?;
    if !output.status.success() {
        bail!(
            "usermod: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Escribe (o sobreescribe) ~/.ssh/authorized_keys con las claves
/// actuales del usuario. Requiere permisos para escribir en home_dir.
pub fn sync_authorized_keys(username: &str, home_dir: &str, keys: &[SshKey]) -> Result<()> {
    use std::fs;

    let ssh_dir = std::path::Path::new(home_dir).join(".ssh");
    fs::create_dir_all(&ssh_dir)?;

    let auth_keys_path = ssh_dir.join("authorized_keys");
    let content: String = keys
        .iter()
        .map(|k| format!("# {}\n{}\n", k.label, k.public_key.trim()))
        .collect();
    fs::write(&auth_keys_path, &content)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&auth_keys_path, fs::Permissions::from_mode(0o600))?;
        fs::set_permissions(&ssh_dir, fs::Permissions::from_mode(0o700))?;
        // chown -R user:user ~/.ssh  (puede fallar si no somos root — mostramos warning)
        let _ = Command::new("chown")
            .args([
                "-R",
                &format!("{}:{}", username, username),
                ssh_dir.to_string_lossy().as_ref(),
            ])
            .output();
    }
    Ok(())
}
