use std::{
    fs::File,
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
    db::ensure_identifier(database)?;
    let creds = creds(cfg, server)?;

    let input: Box<dyn std::io::Read> =
        if dump_path.extension().and_then(|s| s.to_str()) == Some("gz") {
            Box::new(GzDecoder::new(File::open(dump_path)?))
        } else {
            Box::new(File::open(dump_path)?)
        };

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
