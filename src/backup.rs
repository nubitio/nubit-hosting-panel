use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::Utc;
use color_eyre::eyre::{Context, Result, bail, eyre};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use url::Url;

use crate::{config::Config, db, store::DbServer};

struct DbCreds {
    username: String,
    password: Option<String>,
}

pub fn backup(cfg: &Config, server: &DbServer, database: &str, out_dir: &Path) -> Result<PathBuf> {
    db::ensure_identifier(database)?;
    let creds = creds(cfg, server)?;
    let target_dir = out_dir.join(&server.name).join(database);
    std::fs::create_dir_all(&target_dir)?;
    let target = target_dir.join(format!("{}.sql.gz", Utc::now().format("%Y%m%d-%H%M%S")));

    let mut cmd = docker_exec_base(server, creds.password.as_deref());
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
        let _ = std::fs::remove_file(&target);
        bail!("mariadb-dump falló para `{}`", database);
    }

    Ok(target)
}

pub fn restore(cfg: &Config, server: &DbServer, database: &str, dump_path: &Path) -> Result<()> {
    validate_restore_input(dump_path)?;
    db::ensure_identifier(database)?;
    let creds = creds(cfg, server)?;

    let input = open_dump(dump_path)?;

    let mut cmd = docker_exec_base(server, creds.password.as_deref());
    cmd.arg("mariadb")
        .arg("-u")
        .arg(&creds.username)
        .arg(database)
        .stdin(Stdio::piped());

    let mut child = cmd.spawn().wrap_err("ejecutando mariadb vía docker exec")?;
    {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| eyre!("no se pudo abrir stdin de mariadb"))?;
        let mut writer = std::io::BufWriter::new(stdin);
        let mut reader = std::io::BufReader::new(input);
        std::io::copy(&mut reader, &mut writer)?;
    }

    let status = child.wait()?;
    if !status.success() {
        bail!(
            "restore falló para `{}` desde {}",
            database,
            dump_path.display()
        );
    }
    Ok(())
}

pub fn dry_run_restore(
    cfg: &Config,
    server: &DbServer,
    database: &str,
    dump_path: &Path,
) -> Result<()> {
    validate_restore_input(dump_path)?;
    db::ensure_identifier(database)?;
    let _ = creds(cfg, server)?;
    let mut reader = std::io::BufReader::new(open_dump(dump_path)?);
    let mut sink = std::io::sink();
    std::io::copy(&mut reader, &mut sink).wrap_err("leyendo dump completo")?;
    Ok(())
}

pub fn list_backups(
    out_dir: &Path,
    server: Option<&str>,
    database: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let root = match (server, database) {
        (Some(server), Some(database)) => out_dir.join(server).join(database),
        (Some(server), None) => out_dir.join(server),
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
            .map(|name| name.ends_with(".sql") || name.ends_with(".sql.gz"))
            .unwrap_or(false)
        {
            files.push(path);
        }
    }
    Ok(())
}

fn docker_exec_base(server: &DbServer, password: Option<&str>) -> Command {
    let mut cmd = Command::new("docker");
    cmd.arg("exec");
    if let Some(password) = password {
        cmd.arg("-e").arg(format!("MYSQL_PWD={password}"));
    }
    cmd.arg(&server.name);
    cmd
}

fn creds(cfg: &Config, server: &DbServer) -> Result<DbCreds> {
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
