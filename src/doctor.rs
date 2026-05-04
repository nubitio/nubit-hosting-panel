use std::{path::Path, process::Command};

use color_eyre::eyre::Result;

use crate::{config::Config, db, store::Store};

pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

impl Check {
    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: true,
            detail: detail.into(),
        }
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: false,
            detail: detail.into(),
        }
    }
}

pub fn run(cfg: &Config, store: &Store) -> Result<Vec<Check>> {
    let mut checks = Vec::new();

    checks.push(command_exists("docker"));
    checks.push(path_exists("Caddyfile", &cfg.caddyfile_path));
    checks.push(caddy_import_check(cfg));

    if let Some(parent) = cfg.caddy_managed_path.parent() {
        checks.push(path_exists("Caddy managed dir", parent));
    }

    checks.push(container_exists("caddy"));

    for server in store.list_db_servers()? {
        checks.push(container_exists(&server.name));

        if cfg.db_url(&server.name).is_some() {
            checks.push(Check::ok(
                format!("DB credential {}", server.name),
                "env/config presente",
            ));
        } else {
            checks.push(Check::fail(
                format!("DB credential {}", server.name),
                format!(
                    "falta HOSTINGCTL_DB_{}_URL o [db_servers.{}].url",
                    server.name.to_ascii_uppercase().replace('-', "_"),
                    server.name
                ),
            ));
        }

        match db::check_connection(cfg, &server) {
            Ok(()) => checks.push(Check::ok(
                format!("DB connect {}", server.name),
                "SELECT 1 OK",
            )),
            Err(err) => checks.push(Check::fail(
                format!("DB connect {}", server.name),
                err.to_string(),
            )),
        }

        if server.kind == "mssql" {
            checks.push(mssql_sqlcmd_check(cfg, &server.name));
        }
    }

    Ok(checks)
}

pub fn print(checks: &[Check]) {
    for check in checks {
        let marker = if check.ok { "✓" } else { "✗" };
        println!("{} {} — {}", marker, check.name, check.detail);
    }

    let failed = checks.iter().filter(|check| !check.ok).count();
    println!("\n{} checks, {} failed", checks.len(), failed);
}

fn command_exists(command: &str) -> Check {
    match Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {command}"))
        .output()
    {
        Ok(output) if output.status.success() => Check::ok(command, "disponible"),
        _ => Check::fail(command, "no encontrado en PATH"),
    }
}

fn container_exists(container: &str) -> Check {
    match Command::new("docker")
        .arg("inspect")
        .arg(container)
        .output()
    {
        Ok(output) if output.status.success() => {
            Check::ok(format!("container {container}"), "existe")
        }
        Ok(output) => Check::fail(
            format!("container {container}"),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ),
        Err(err) => Check::fail(format!("container {container}"), err.to_string()),
    }
}

fn path_exists(name: &str, path: &Path) -> Check {
    if path.exists() {
        Check::ok(name, path.display().to_string())
    } else {
        Check::fail(name, format!("no existe: {}", path.display()))
    }
}

fn mssql_sqlcmd_check(cfg: &Config, container: &str) -> Check {
    let path = &cfg.mssql_sqlcmd_path;
    match Command::new("docker")
        .args(["exec", container, "test", "-x", path])
        .output()
    {
        Ok(output) if output.status.success() => {
            Check::ok(format!("sqlcmd {container}"), path.clone())
        }
        _ => {
            // Try go-sqlcmd fallback
            match Command::new("docker")
                .args(["exec", container, "which", "sqlcmd"])
                .output()
            {
                Ok(output) if output.status.success() => {
                    let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    Check::ok(
                        format!("sqlcmd {container}"),
                        format!("{found} (go-sqlcmd; actualiza mssql_sqlcmd_path en config.toml)"),
                    )
                }
                _ => Check::fail(
                    format!("sqlcmd {container}"),
                    format!("no encontrado en {path}; verifica mssql_sqlcmd_path en config.toml"),
                ),
            }
        }
    }
}

fn caddy_import_check(cfg: &Config) -> Check {
    let import_name = cfg
        .caddy_managed_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("hostingctl.caddy");
    let import_line = format!("import {import_name}");

    match std::fs::read_to_string(&cfg.caddyfile_path) {
        Ok(raw) if raw.lines().any(|line| line.trim() == import_line) => {
            Check::ok("Caddy import", import_line)
        }
        Ok(_) => Check::fail("Caddy import", format!("falta `{import_line}`")),
        Err(err) => Check::fail("Caddy import", err.to_string()),
    }
}
