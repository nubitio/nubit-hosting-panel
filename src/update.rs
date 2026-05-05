use color_eyre::eyre::{Result, bail, eyre};
use std::io::Write as _;
use std::process::Command;

const REPO: &str = "nubitio/nubit-hosting-panel";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(check_only: bool, force: bool) -> Result<()> {
    let current_tag = format!("v{CURRENT_VERSION}");

    print!("hostingctl {current_tag} — verificando última versión en GitHub… ");
    let _ = std::io::stdout().flush();

    let latest = fetch_latest_tag()?;
    println!("{latest}");

    let up_to_date = latest == current_tag;

    if check_only {
        if up_to_date {
            println!("✓ Ya estás en la versión más reciente.");
        } else {
            println!("→ Actualización disponible: {current_tag} → {latest}");
            println!("  Ejecuta: hostingctl update");
        }
        return Ok(());
    }

    if up_to_date && !force {
        println!("✓ Ya estás en la versión más reciente. Usa --force para reinstalar.");
        return Ok(());
    }

    if up_to_date {
        println!("Re-instalando {latest} (--force)…");
    } else {
        println!("Actualizando {current_tag} → {latest}…");
    }
    println!();

    // Detectar el directorio de instalación desde la ubicación del binario actual
    let current_exe = std::env::current_exe()?;
    let bin_dir = current_exe
        .parent()
        .ok_or_else(|| eyre!("no se pudo determinar el directorio del binario"))?;
    // --prefix apunta un nivel arriba de /bin (ej: /usr/local si binario en /usr/local/bin)
    let prefix = bin_dir.parent().unwrap_or(bin_dir);

    let install_url =
        format!("https://github.com/{REPO}/releases/latest/download/install.sh");
    let cmd = format!(
        "curl -fsSL '{}' | sh -s -- --prefix='{}'",
        install_url,
        prefix.display()
    );

    let status = Command::new("sh").arg("-c").arg(&cmd).status()?;

    if !status.success() {
        bail!("el script de instalación falló");
    }

    println!();
    println!("✓ hostingctl actualizado a {latest}");
    Ok(())
}

fn fetch_latest_tag() -> Result<String> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "-H",
            "Accept: application/vnd.github+json",
            &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        ])
        .output()?;

    if !output.status.success() {
        bail!(
            "curl falló ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let body = String::from_utf8_lossy(&output.stdout);
    extract_tag(&body)
        .ok_or_else(|| eyre!("no se encontró tag_name en la respuesta de GitHub"))
}

/// Extrae el valor de "tag_name" de la respuesta JSON de la API de GitHub.
/// Formato esperado: `"tag_name": "v0.1.24"`
fn extract_tag(body: &str) -> Option<String> {
    let needle = "\"tag_name\":";
    let start = body.find(needle)? + needle.len();
    let s = body[start..].trim_start_matches([' ', '\t', '\n', '\r']);
    let s = s.strip_prefix('"')?;
    let end = s.find('"')?;
    Some(s[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tag_parses_github_response() {
        let body = r#"{"url":"https://api.github.com/repos/x/y/releases/1","tag_name": "v0.1.24","name":"v0.1.24"}"#;
        assert_eq!(extract_tag(body).as_deref(), Some("v0.1.24"));
    }

    #[test]
    fn extract_tag_returns_none_on_missing_key() {
        assert!(extract_tag(r#"{"name":"foo"}"#).is_none());
    }
}
