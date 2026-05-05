use std::{fs, process::Command};

use color_eyre::eyre::{Context, Result, eyre};

use crate::{config::Config, store::{App, DomainAlias}};

pub fn render_block(apps: &[App], aliases: &[DomainAlias]) -> String {
    let mut out = String::new();
    out.push_str("# This file is managed by hostingctl. Do not edit manually.\n\n");
    for app in apps {
        let extra: Vec<&str> = aliases
            .iter()
            .filter(|a| a.app_id == app.id)
            .map(|a| a.domain.as_str())
            .collect();
        let domains = if extra.is_empty() {
            app.domain.clone()
        } else {
            format!("{} {}", app.domain, extra.join(" "))
        };
        out.push_str(&format!(
            "# client: {client} app: {slug}\n{domains} {{\n  encode zstd gzip\n  reverse_proxy {upstream}\n}}\n\n",
            client = app.client_slug,
            slug = app.slug,
            domains = domains,
            upstream = app.upstream,
        ));
    }
    out
}

pub fn bootstrap(cfg: &Config) -> Result<bool> {
    let path = &cfg.caddyfile_path;
    let import_line = format!("import {}", import_target(cfg));
    let existing = fs::read_to_string(path).unwrap_or_default();

    if existing.lines().any(|line| line.trim() == import_line) {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        backup(path)?;
    }

    let mut next = existing.trim_end().to_string();
    if !next.is_empty() {
        next.push_str("\n\n");
    }
    next.push_str(&import_line);
    next.push('\n');
    fs::write(path, next).wrap_err_with(|| format!("escribiendo {}", path.display()))?;
    Ok(true)
}

pub fn apply(cfg: &Config, apps: &[App], aliases: &[DomainAlias], reload: bool) -> Result<()> {
    let path = &cfg.caddy_managed_path;
    let previous = fs::read_to_string(path).ok();
    let next = render_block(apps, aliases);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        backup(path)?;
    }

    fs::write(path, next).wrap_err_with(|| format!("escribiendo {}", path.display()))?;

    if let Err(err) = validate(cfg) {
        match previous {
            Some(previous) => fs::write(path, previous)?,
            None => {
                let _ = fs::remove_file(path);
            }
        }
        return Err(err.wrap_err("Caddy validate falló; se restauró la config anterior"));
    }

    if reload {
        run_command(
            cfg.caddy_reload_command.as_deref(),
            "caddy_reload_command no está configurado",
            "falló reload de Caddy",
        )?;
    }

    Ok(())
}

fn validate(cfg: &Config) -> Result<()> {
    run_command(
        cfg.caddy_validate_command.as_deref(),
        "caddy_validate_command no está configurado",
        "falló validate de Caddy",
    )
}

fn run_command(command: Option<&str>, missing: &str, failed: &str) -> Result<()> {
    let command = command.ok_or_else(|| eyre!(missing.to_string()))?;
    let status = Command::new("sh").arg("-lc").arg(command).status()?;
    if !status.success() {
        return Err(eyre!("{failed}: {command}"));
    }
    Ok(())
}

fn backup(path: &std::path::Path) -> Result<()> {
    let backup = path.with_extension(format!(
        "backup-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    ));
    fs::copy(path, &backup).wrap_err_with(|| format!("creando backup {}", backup.display()))?;
    Ok(())
}

fn import_target(cfg: &Config) -> String {
    cfg.caddy_managed_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("hostingctl.caddy")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::App;
    use chrono::Utc;

    #[test]
    fn render_block_contains_app_proxy() {
        let rendered = render_block(&[App {
            id: "app-1".into(),
            client_slug: "porteroseguro".into(),
            slug: "web".into(),
            domain: "porteroseguro.nubit.site".into(),
            upstream: "tomcat_porteroseguro:8080".into(),
            created_at: Utc::now(),
        }], &[]);

        assert!(rendered.contains("# This file is managed by hostingctl"));
        assert!(rendered.contains("porteroseguro.nubit.site"));
        assert!(rendered.contains("reverse_proxy tomcat_porteroseguro:8080"));
    }
}
