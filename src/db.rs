use color_eyre::eyre::{Result, bail, eyre};
use mysql::{Opts, Pool, prelude::Queryable};

use crate::{
    config::Config,
    store::{DbServer, Store, validate_slug},
};

pub struct ProvisionedDb {
    pub database: String,
    pub username: String,
    pub host: String,
    pub password: String,
}

pub fn convention_names(client: &str, app: &str, env: &str) -> Result<(String, String)> {
    validate_slug(client)?;
    validate_slug(app)?;
    validate_slug(env)?;
    Ok((
        format!(
            "{}_{}_{}",
            client.replace('-', "_"),
            app.replace('-', "_"),
            env.replace('-', "_")
        ),
        format!(
            "{}_{}_user",
            client.replace('-', "_"),
            app.replace('-', "_")
        ),
    ))
}

pub fn create_database(cfg: &Config, server: &DbServer, name: &str) -> Result<()> {
    ensure_mariadb(server)?;
    ensure_identifier(name)?;
    let pool = pool(cfg, server)?;
    let mut conn = pool.get_conn()?;
    conn.query_drop(format!(
        "CREATE DATABASE IF NOT EXISTS `{}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci",
        name
    ))?;
    Ok(())
}

pub fn create_user(
    cfg: &Config,
    server: &DbServer,
    username: &str,
    host: &str,
    password: &str,
) -> Result<()> {
    ensure_mariadb(server)?;
    ensure_identifier(username)?;
    ensure_host(host)?;
    let pool = pool(cfg, server)?;
    let mut conn = pool.get_conn()?;
    conn.query_drop(format!(
        "CREATE USER IF NOT EXISTS '{}'@'{}' IDENTIFIED BY '{}'",
        sql_string(username),
        sql_string(host),
        sql_string(password)
    ))?;
    Ok(())
}

pub fn reset_password(
    cfg: &Config,
    server: &DbServer,
    username: &str,
    host: &str,
    password: &str,
) -> Result<()> {
    ensure_mariadb(server)?;
    ensure_identifier(username)?;
    ensure_host(host)?;
    let pool = pool(cfg, server)?;
    let mut conn = pool.get_conn()?;
    conn.query_drop(format!(
        "ALTER USER '{}'@'{}' IDENTIFIED BY '{}'",
        sql_string(username),
        sql_string(host),
        sql_string(password)
    ))?;
    Ok(())
}

pub fn grant_all(
    cfg: &Config,
    store: &Store,
    server: &DbServer,
    client_slug: &str,
    app_slug: Option<&str>,
    env: &str,
    db_name: &str,
    username: &str,
    host: &str,
) -> Result<()> {
    ensure_mariadb(server)?;
    ensure_identifier(db_name)?;
    ensure_identifier(username)?;
    ensure_host(host)?;
    let pool = pool(cfg, server)?;
    let mut conn = pool.get_conn()?;
    conn.query_drop(format!(
        "GRANT ALL PRIVILEGES ON `{}`.* TO '{}'@'{}'",
        db_name,
        sql_string(username),
        sql_string(host)
    ))?;
    conn.query_drop("FLUSH PRIVILEGES")?;
    store.record_grant(
        &server.id,
        client_slug,
        app_slug,
        env,
        db_name,
        username,
        host,
    )?;
    Ok(())
}

pub fn provision(
    cfg: &Config,
    store: &Store,
    server: &DbServer,
    client: &str,
    app: &str,
    env: &str,
    host: &str,
    database: Option<&str>,
    username: Option<&str>,
    password: String,
) -> Result<ProvisionedDb> {
    let (default_db, default_user) = convention_names(client, app, env)?;
    let database = database.unwrap_or(&default_db).to_string();
    let username = username.unwrap_or(&default_user).to_string();

    create_database(cfg, server, &database)?;
    create_user(cfg, server, &username, host, &password)?;
    grant_all(
        cfg,
        store,
        server,
        client,
        Some(app),
        env,
        &database,
        &username,
        host,
    )?;

    Ok(ProvisionedDb {
        database,
        username,
        host: host.to_string(),
        password,
    })
}

fn pool(cfg: &Config, server: &DbServer) -> Result<Pool> {
    let url = cfg.db_url(&server.name).ok_or_else(|| {
        eyre!(
            "no hay credencial para DB server `{}`; define HOSTINGCTL_DB_{}_URL o [db_servers.{}].url en config.toml",
            server.name,
            server.name.to_ascii_uppercase().replace('-', "_"),
            server.name,
        )
    })?;
    Ok(Pool::new(Opts::from_url(&url)?)?)
}

fn ensure_mariadb(server: &DbServer) -> Result<()> {
    if server.kind != "mariadb" && server.kind != "mysql" {
        bail!(
            "por ahora solo soportamos mariadb/mysql; recibido: {}",
            server.kind
        );
    }
    Ok(())
}

fn ensure_identifier(value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 64
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        bail!(
            "identificador inválido `{}`; usa solo letras, números y _",
            value
        );
    }
    Ok(())
}

fn ensure_host(value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 255
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '%' | ':'));
    if !ok {
        bail!("host inválido `{}`", value);
    }
    Ok(())
}

fn sql_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "''")
}
