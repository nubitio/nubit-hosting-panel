use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::Utc;
use color_eyre::eyre::{Context, Result, bail, eyre};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use url::Url;

use crate::{
    config::Config,
    mssql,
    store::{DbServer, ensure_identifier},
};

struct DbCreds {
    username: String,
    password: Option<String>,
}

// ── Public dispatch ───────────────────────────────────────────────────────────

pub fn backup(cfg: &Config, server: &DbServer, database: &str, out_dir: &Path) -> Result<PathBuf> {
    match server.kind.as_str() {
        "mariadb" | "mysql" => mariadb_backup(cfg, server, database, out_dir),
        "mssql" => mssql_backup(cfg, server, database, out_dir),
        k => bail!("DB kind no soportado para backup: {}", k),
    }
}

pub fn restore(cfg: &Config, server: &DbServer, database: &str, dump_path: &Path) -> Result<()> {
    match server.kind.as_str() {
        "mariadb" | "mysql" => mariadb_restore(cfg, server, database, dump_path),
        "mssql" => mssql_restore(cfg, server, database, dump_path),
        k => bail!("DB kind no soportado para restore: {}", k),
    }
}

pub fn dry_run_restore(
    cfg: &Config,
    server: &DbServer,
    database: &str,
    dump_path: &Path,
) -> Result<()> {
    match server.kind.as_str() {
        "mariadb" | "mysql" => mariadb_dry_run(cfg, server, database, dump_path),
        "mssql" => mssql_dry_run(server, database, dump_path),
        k => bail!("DB kind no soportado para dry-run: {}", k),
    }
}

pub fn list_backups(
    out_dir: &Path,
    server: Option<&str>,
    database: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let root = match (server, database) {
        (Some(s), Some(d)) => out_dir.join(s).join(d),
        (Some(s), None) => out_dir.join(s),
        (None, _) => out_dir.to_path_buf(),
    };
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect_dumps(&root, &mut files)?;
    files.sort();
    Ok(files)
}

// ── MariaDB backup / restore ──────────────────────────────────────────────────

fn mariadb_backup(
    cfg: &Config,
    server: &DbServer,
    database: &str,
    out_dir: &Path,
) -> Result<PathBuf> {
    ensure_identifier(database)?;
    let creds = mariadb_creds(cfg, server)?;
    let target_dir = out_dir.join(&server.name).join(database);
    fs::create_dir_all(&target_dir)?;
    let target = target_dir.join(format!("{}.sql.gz", Utc::now().format("%Y%m%d-%H%M%S")));

    let mut cmd = docker_exec_base(server, creds.password.as_deref(), "MYSQL_PWD");
    cmd.arg("mariadb-dump")
        .arg("--single-transaction")
        .arg("--routines")
        .arg("--triggers")
        .arg("--events")
        .arg("-u")
        .arg(&creds.username)
        .arg(database)
        .stdout(Stdio::piped());

    let mut child = cmd
        .spawn()
        .wrap_err("ejecutando mariadb-dump vía docker exec")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| eyre!("no se pudo leer stdout de mariadb-dump"))?;
    let mut reader = std::io::BufReader::new(stdout);
    let file = File::create(&target).wrap_err_with(|| format!("creando {}", target.display()))?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    std::io::copy(&mut reader, &mut encoder)?;
    encoder.finish()?;

    let status = child.wait()?;
    if !status.success() {
        let _ = fs::remove_file(&target);
        bail!("mariadb-dump falló para `{}`", database);
    }
    Ok(target)
}

fn mariadb_restore(
    cfg: &Config,
    server: &DbServer,
    database: &str,
    dump_path: &Path,
) -> Result<()> {
    validate_restore_input(dump_path)?;
    ensure_identifier(database)?;
    let creds = mariadb_creds(cfg, server)?;
    let input = open_dump(dump_path)?;

    let mut cmd = docker_exec_base(server, creds.password.as_deref(), "MYSQL_PWD");
    cmd.arg("mariadb")
        .arg("--one-database") // ignora USE statements del dump; fuerza todo a la DB target
        .arg("-u")
        .arg(&creds.username)
        .arg(database)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().wrap_err("ejecutando mariadb vía docker exec")?;
    {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| eyre!("no se pudo abrir stdin de mariadb"))?;
        let mut writer = std::io::BufWriter::new(stdin);
        let mut reader = std::io::BufReader::new(input);
        match std::io::copy(&mut reader, &mut writer) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                // mariadb cerró stdin antes de leer todo — el exit code dirá si falló
            }
            Err(e) => return Err(e).wrap_err("escribiendo dump a stdin de mariadb"),
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            bail!(
                "restore falló para `{}` desde {} (sin stderr)",
                database,
                dump_path.display()
            );
        } else {
            bail!(
                "restore falló para `{}` desde {}:\n{}",
                database,
                dump_path.display(),
                stderr
            );
        }
    }
    Ok(())
}

fn mariadb_dry_run(
    cfg: &Config,
    server: &DbServer,
    database: &str,
    dump_path: &Path,
) -> Result<()> {
    validate_restore_input(dump_path)?;
    ensure_identifier(database)?;
    let _ = mariadb_creds(cfg, server)?;
    let mut reader = std::io::BufReader::new(open_dump(dump_path)?);
    let mut sink = std::io::sink();
    std::io::copy(&mut reader, &mut sink).wrap_err("leyendo dump completo")?;
    Ok(())
}

// ── MSSQL backup / restore ────────────────────────────────────────────────────
//
// Strategy:
//   backup:  docker exec sqlcmd BACKUP DATABASE → /tmp/hctl_<db>.bak
//            docker cp container:/tmp/hctl_<db>.bak → out_dir/<db>.bak
//            docker exec rm /tmp/hctl_<db>.bak
//   restore: docker cp dump → container:/tmp/hctl_restore.bak
//            docker exec sqlcmd RESTORE DATABASE
//            docker exec rm /tmp/hctl_restore.bak

fn mssql_backup(
    cfg: &Config,
    server: &DbServer,
    database: &str,
    out_dir: &Path,
) -> Result<PathBuf> {
    ensure_identifier(database)?;
    let url = mssql::get_url(cfg, server)?;
    let parsed = Url::parse(&url)?;
    let sa_user = parsed.username().to_string();
    let sa_pass = parsed.password().unwrap_or("").to_string();

    let target_dir = out_dir.join(&server.name).join(database);
    fs::create_dir_all(&target_dir)?;
    let ts = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let container_path = format!("/tmp/hctl_{database}_{ts}.bak");
    let local_path = target_dir.join(format!("{ts}.bak"));

    let sqlcmd = &cfg.mssql_sqlcmd_path;

    // Execute BACKUP DATABASE inside container
    let status = Command::new("docker")
        .arg("exec")
        .arg("-e")
        .arg(format!("SQLCMDPASSWORD={sa_pass}"))
        .arg(&server.name)
        .arg(sqlcmd)
        .arg("-S")
        .arg("localhost")
        .arg("-U")
        .arg(&sa_user)
        .arg("-Q")
        .arg(format!(
            "BACKUP DATABASE [{database}] TO DISK = N'{container_path}' WITH INIT, STATS = 10"
        ))
        .status()
        .wrap_err("ejecutando sqlcmd BACKUP vía docker exec")?;
    if !status.success() {
        bail!("BACKUP DATABASE falló para `{}`", database);
    }

    // docker cp to extract the .bak
    let status = Command::new("docker")
        .arg("cp")
        .arg(format!("{}:{}", server.name, container_path))
        .arg(&local_path)
        .status()
        .wrap_err("docker cp extrayendo backup")?;
    if !status.success() {
        bail!("docker cp falló extrayendo backup de `{}`", database);
    }

    // cleanup inside container
    let _ = Command::new("docker")
        .arg("exec")
        .arg(&server.name)
        .arg("rm")
        .arg("-f")
        .arg(&container_path)
        .status();

    Ok(local_path)
}

fn mssql_restore(cfg: &Config, server: &DbServer, database: &str, dump_path: &Path) -> Result<()> {
    ensure_identifier(database)?;
    if !dump_path.exists() {
        bail!("dump no existe: {}", dump_path.display());
    }

    let url = mssql::get_url(cfg, server)?;
    let parsed = Url::parse(&url)?;
    let sa_user = parsed.username().to_string();
    let sa_pass = parsed.password().unwrap_or("").to_string();

    let sqlcmd = &cfg.mssql_sqlcmd_path;
    let container_path = format!("/tmp/hctl_restore_{database}.bak");

    // Copy .bak into container
    let status = Command::new("docker")
        .arg("cp")
        .arg(dump_path)
        .arg(format!("{}:{}", server.name, container_path))
        .status()
        .wrap_err("docker cp copiando backup al contenedor")?;
    if !status.success() {
        bail!("docker cp falló copiando backup al contenedor");
    }

    // Execute RESTORE DATABASE
    let status = Command::new("docker")
        .arg("exec")
        .arg("-e")
        .arg(format!("SQLCMDPASSWORD={sa_pass}"))
        .arg(&server.name)
        .arg(sqlcmd)
        .arg("-S")
        .arg("localhost")
        .arg("-U")
        .arg(&sa_user)
        .arg("-Q")
        .arg(format!(
            "RESTORE DATABASE [{database}] FROM DISK = N'{container_path}' WITH REPLACE, STATS = 10"
        ))
        .status()
        .wrap_err("ejecutando sqlcmd RESTORE vía docker exec")?;

    // cleanup regardless
    let _ = Command::new("docker")
        .arg("exec")
        .arg(&server.name)
        .arg("rm")
        .arg("-f")
        .arg(&container_path)
        .status();

    if !status.success() {
        bail!("RESTORE DATABASE falló para `{}`", database);
    }
    Ok(())
}

fn mssql_dry_run(server: &DbServer, database: &str, dump_path: &Path) -> Result<()> {
    ensure_identifier(database)?;
    if !dump_path.exists() {
        bail!("dump no existe: {}", dump_path.display());
    }
    if !dump_path.is_file() {
        bail!("dump no es archivo: {}", dump_path.display());
    }
    let name = dump_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if !name.ends_with(".bak") {
        bail!("MSSQL dump debe terminar en .bak: {}", dump_path.display());
    }
    // Check container exists
    let status = Command::new("docker")
        .arg("inspect")
        .arg(&server.name)
        .status()?;
    if !status.success() {
        bail!("contenedor `{}` no encontrado", server.name);
    }
    Ok(())
}

// ── Common helpers ────────────────────────────────────────────────────────────

fn validate_restore_input(dump_path: &Path) -> Result<()> {
    if !dump_path.exists() {
        bail!("dump no existe: {}", dump_path.display());
    }
    if !dump_path.is_file() {
        bail!("dump no es archivo: {}", dump_path.display());
    }
    let name = dump_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if !(name.ends_with(".sql") || name.ends_with(".sql.gz")) {
        bail!(
            "dump debe terminar en .sql o .sql.gz: {}",
            dump_path.display()
        );
    }
    Ok(())
}

fn open_dump(dump_path: &Path) -> Result<Box<dyn std::io::Read>> {
    if dump_path.extension().and_then(|s| s.to_str()) == Some("gz") {
        Ok(Box::new(GzDecoder::new(File::open(dump_path)?)))
    } else {
        Ok(Box::new(File::open(dump_path)?))
    }
}

fn collect_dumps(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_dumps(&path, files)?;
        } else if path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|name| {
                name.ends_with(".sql") || name.ends_with(".sql.gz") || name.ends_with(".bak")
            })
            .unwrap_or(false)
        {
            files.push(path);
        }
    }
    Ok(())
}

fn docker_exec_base(server: &DbServer, password: Option<&str>, env_var: &str) -> Command {
    let mut cmd = Command::new("docker");
    cmd.arg("exec");
    if let Some(pw) = password {
        cmd.arg("-e").arg(format!("{env_var}={pw}"));
    }
    cmd.arg(&server.name);
    cmd
}

fn mariadb_creds(cfg: &Config, server: &DbServer) -> Result<DbCreds> {
    let url = cfg.db_url(&server.name).ok_or_else(|| {
        eyre!(
            "no hay credencial para DB server `{}`; define HOSTINGCTL_DB_{}_URL",
            server.name,
            server.name.to_ascii_uppercase().replace('-', "_")
        )
    })?;
    let parsed = Url::parse(&url)?;
    Ok(DbCreds {
        username: parsed.username().to_string(),
        password: parsed.password().map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_backups_finds_sql_sql_gz_and_bak() {
        let root = std::env::temp_dir().join(format!("hostingctl-test-{}", uuid::Uuid::new_v4()));
        let db_dir = root.join("mariadb").join("app_db");
        fs::create_dir_all(&db_dir).unwrap();
        fs::write(db_dir.join("a.sql"), "-- dump").unwrap();
        fs::write(db_dir.join("b.sql.gz"), "gz").unwrap();
        fs::write(db_dir.join("c.bak"), "bak").unwrap();
        fs::write(db_dir.join("ignore.txt"), "x").unwrap();

        let files = list_backups(&root, Some("mariadb"), Some("app_db")).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();

        assert_eq!(names, vec!["a.sql", "b.sql.gz", "c.bak"]);
        fs::remove_dir_all(root).unwrap();
    }
}
