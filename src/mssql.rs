use color_eyre::eyre::{Result, eyre};
use tiberius::{AuthMethod, Client, Config as TibConfig};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;
use url::Url;

use crate::{
    config::Config,
    store::{DbServer, Store, ensure_identifier},
};

type MssqlClient = Client<tokio_util::compat::Compat<TcpStream>>;

// ── Runtime helper ────────────────────────────────────────────────────────────

fn block_on<F, T>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| eyre!("no se pudo crear tokio runtime: {}", e))?
        .block_on(fut)
}

// ── Connection ────────────────────────────────────────────────────────────────

async fn connect(url: &str) -> Result<MssqlClient> {
    let parsed = Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("localhost").to_string();
    let port = parsed.port().unwrap_or(1433);
    let user = parsed.username().to_string();
    let pass = parsed.password().unwrap_or("").to_string();

    let mut config = TibConfig::new();
    config.host(&host);
    config.port(port);
    config.authentication(AuthMethod::sql_server(&user, &pass));
    config.trust_cert();

    let tcp = TcpStream::connect((host.as_str(), port)).await?;
    tcp.set_nodelay(true)?;
    Ok(Client::connect(config, tcp.compat_write()).await?)
}

pub fn get_url(cfg: &Config, server: &DbServer) -> Result<String> {
    cfg.db_url(&server.name).ok_or_else(|| {
        eyre!(
            "no hay credencial para `{}`; define HOSTINGCTL_DB_{}_URL",
            server.name,
            server.name.to_ascii_uppercase().replace('-', "_")
        )
    })
}

// ── Operations ────────────────────────────────────────────────────────────────

pub fn check_connection(cfg: &Config, server: &DbServer) -> Result<()> {
    let url = get_url(cfg, server)?;
    block_on(async move {
        let mut c = connect(&url).await?;
        c.execute("SELECT 1", &[]).await?;
        Ok(())
    })
}

pub fn create_database(cfg: &Config, server: &DbServer, name: &str) -> Result<()> {
    ensure_identifier(name)?;
    let url = get_url(cfg, server)?;
    let name = name.to_string();
    block_on(async move {
        let mut c = connect(&url).await?;
        c.execute(
            &format!(
                "IF NOT EXISTS (SELECT name FROM sys.databases WHERE name = N'{name}') \
                 CREATE DATABASE [{name}]"
            ),
            &[],
        )
        .await?;
        Ok(())
    })
}

pub fn create_login(cfg: &Config, server: &DbServer, username: &str, password: &str) -> Result<()> {
    ensure_identifier(username)?;
    let url = get_url(cfg, server)?;
    let username = username.to_string();
    let password = escape_mssql(password);
    block_on(async move {
        let mut c = connect(&url).await?;
        c.execute(
            &format!(
                "IF NOT EXISTS (SELECT name FROM sys.server_principals WHERE name = N'{username}') \
                 CREATE LOGIN [{username}] WITH PASSWORD = N'{password}'"
            ),
            &[],
        )
        .await?;
        Ok(())
    })
}

pub fn grant_dbo(
    cfg: &Config,
    store: &Store,
    server: &DbServer,
    client_slug: &str,
    app_slug: Option<&str>,
    env: &str,
    db_name: &str,
    username: &str,
) -> Result<()> {
    ensure_identifier(db_name)?;
    ensure_identifier(username)?;
    let url = get_url(cfg, server)?;
    let db_name_str = db_name.to_string();
    let username_str = username.to_string();
    block_on(async move {
        let mut c = connect(&url).await?;
        c.execute(
            &format!(
                "USE [{db_name_str}]; \
                 IF NOT EXISTS (SELECT name FROM sys.database_principals WHERE name = N'{username_str}') \
                 CREATE USER [{username_str}] FOR LOGIN [{username_str}]"
            ),
            &[],
        )
        .await?;
        c.execute(
            &format!("USE [{db_name_str}]; ALTER ROLE [db_owner] ADD MEMBER [{username_str}]"),
            &[],
        )
        .await?;
        Ok(())
    })?;
    store.record_grant(
        &server.id,
        client_slug,
        app_slug,
        env,
        db_name,
        username,
        "server",
    )?;
    Ok(())
}

pub fn reset_password(
    cfg: &Config,
    server: &DbServer,
    username: &str,
    password: &str,
) -> Result<()> {
    ensure_identifier(username)?;
    let url = get_url(cfg, server)?;
    let username = username.to_string();
    let password = escape_mssql(password);
    block_on(async move {
        let mut c = connect(&url).await?;
        c.execute(
            &format!("ALTER LOGIN [{username}] WITH PASSWORD = N'{password}'"),
            &[],
        )
        .await?;
        Ok(())
    })
}

fn escape_mssql(value: &str) -> String {
    value.replace('\'', "''")
}
