use color_eyre::eyre::{Result, bail, eyre};
use mysql::{Opts, Pool, prelude::Queryable};

use crate::{
    config::Config,
    mssql,
    store::{DbServer, ProvisionedDb, Store, convention_names, ensure_identifier},
};

// ── Public dispatch ───────────────────────────────────────────────────────────

pub fn check_connection(cfg: &Config, server: &DbServer) -> Result<()> {
    match server.kind.as_str() {
        "mariadb" | "mysql" => mariadb_check_connection(cfg, server),
        "mssql" => mssql::check_connection(cfg, server),
        k => bail!("DB kind no soportado: {}", k),
    }
}

pub fn create_database(cfg: &Config, server: &DbServer, name: &str) -> Result<()> {
    match server.kind.as_str() {
        "mariadb" | "mysql" => mariadb_create_database(cfg, server, name),
        "mssql" => mssql::create_database(cfg, server, name),
        k => bail!("DB kind no soportado: {}", k),
    }
}

pub fn create_user(
    cfg: &Config,
    server: &DbServer,
    username: &str,
    host: &str,
    password: &str,
) -> Result<()> {
    match server.kind.as_str() {
        "mariadb" | "mysql" => mariadb_create_user(cfg, server, username, host, password),
        "mssql" => mssql::create_login(cfg, server, username, password),
        k => bail!("DB kind no soportado: {}", k),
    }
}

pub fn reset_password(
    cfg: &Config,
    server: &DbServer,
    username: &str,
    host: &str,
    password: &str,
) -> Result<()> {
    match server.kind.as_str() {
        "mariadb" | "mysql" => mariadb_reset_password(cfg, server, username, host, password),
        "mssql" => mssql::reset_password(cfg, server, username, password),
        k => bail!("DB kind no soportado: {}", k),
    }
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
    match server.kind.as_str() {
        "mariadb" | "mysql" => mariadb_grant_all(
            cfg,
            store,
            server,
            client_slug,
            app_slug,
            env,
            db_name,
            username,
            host,
        ),
        "mssql" => mssql::grant_dbo(
            cfg,
            store,
            server,
            client_slug,
            app_slug,
            env,
            db_name,
            username,
        ),
        k => bail!("DB kind no soportado: {}", k),
    }
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

    match server.kind.as_str() {
        "mariadb" | "mysql" => {
            mariadb_create_database(cfg, server, &database)?;
            mariadb_create_user(cfg, server, &username, host, &password)?;
            mariadb_grant_all(
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
        "mssql" => {
            mssql::create_database(cfg, server, &database)?;
            mssql::create_login(cfg, server, &username, &password)?;
            mssql::grant_dbo(
                cfg,
                store,
                server,
                client,
                Some(app),
                env,
                &database,
                &username,
            )?;
            Ok(ProvisionedDb {
                database,
                username,
                host: "server".to_string(),
                password,
            })
        }
        k => bail!("DB kind no soportado: {}", k),
    }
}

// ── MariaDB internals ─────────────────────────────────────────────────────────

fn mariadb_check_connection(cfg: &Config, server: &DbServer) -> Result<()> {
    let pool = mariadb_pool(cfg, server)?;
    let mut conn = pool.get_conn()?;
    conn.query_drop("SELECT 1")?;
    Ok(())
}

fn mariadb_create_database(cfg: &Config, server: &DbServer, name: &str) -> Result<()> {
    ensure_identifier(name)?;
    let pool = mariadb_pool(cfg, server)?;
    let mut conn = pool.get_conn()?;
    conn.query_drop(format!(
        "CREATE DATABASE IF NOT EXISTS `{}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci",
        name
    ))?;
    Ok(())
}

fn mariadb_create_user(
    cfg: &Config,
    server: &DbServer,
    username: &str,
    host: &str,
    password: &str,
) -> Result<()> {
    ensure_identifier(username)?;
    ensure_host(host)?;
    let pool = mariadb_pool(cfg, server)?;
    let mut conn = pool.get_conn()?;
    conn.query_drop(format!(
        "CREATE USER IF NOT EXISTS '{}'@'{}' IDENTIFIED BY '{}'",
        sql_string(username),
        sql_string(host),
        sql_string(password)
    ))?;
    Ok(())
}

fn mariadb_reset_password(
    cfg: &Config,
    server: &DbServer,
    username: &str,
    host: &str,
    password: &str,
) -> Result<()> {
    ensure_identifier(username)?;
    ensure_host(host)?;
    let pool = mariadb_pool(cfg, server)?;
    let mut conn = pool.get_conn()?;
    conn.query_drop(format!(
        "ALTER USER '{}'@'{}' IDENTIFIED BY '{}'",
        sql_string(username),
        sql_string(host),
        sql_string(password)
    ))?;
    Ok(())
}

fn mariadb_grant_all(
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
    ensure_identifier(db_name)?;
    ensure_identifier(username)?;
    ensure_host(host)?;
    let pool = mariadb_pool(cfg, server)?;
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

fn mariadb_pool(cfg: &Config, server: &DbServer) -> Result<Pool> {
    let url = cfg.db_url(&server.name).ok_or_else(|| {
        eyre!(
            "no hay credencial para DB server `{}`; define HOSTINGCTL_DB_{}_URL",
            server.name,
            server.name.to_ascii_uppercase().replace('-', "_"),
        )
    })?;
    Ok(Pool::new(Opts::from_url(&url)?)?)
}

// ── Common helpers ────────────────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convention_names_defaults_to_underscored_identifiers() {
        let (db, user) = convention_names("portero-seguro", "web", "prod").unwrap();
        assert_eq!(db, "portero_seguro_web_prod");
        assert_eq!(user, "portero_seguro_web_user");
    }

    #[test]
    fn convention_names_rejects_invalid_slug() {
        assert!(convention_names("Portero", "web", "prod").is_err());
    }
}
