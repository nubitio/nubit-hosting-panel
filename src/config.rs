use std::{collections::HashMap, fs, path::PathBuf};

use color_eyre::eyre::{Context, Result, eyre};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub data_dir: PathBuf,
    pub caddyfile_path: PathBuf,
    pub caddy_managed_path: PathBuf,
    pub caddy_validate_command: Option<String>,
    pub caddy_reload_command: Option<String>,
    #[serde(default = "default_sqlcmd_path")]
    pub mssql_sqlcmd_path: String,
    #[serde(default)]
    pub db_servers: HashMap<String, DbServerSecret>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbServerSecret {
    pub url: String,
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        let dir = dirs::config_dir()
            .ok_or_else(|| eyre!("no se pudo resolver el directorio de configuración"))?
            .join("nubit-hosting-panel");
        Ok(dir.join("config.toml"))
    }

    pub fn load_or_create() -> Result<Self> {
        let path = Self::default_path()?;
        if !path.exists() {
            let cfg = Self::default()?;
            cfg.save()?;
            return Ok(cfg);
        }
        let raw =
            fs::read_to_string(&path).wrap_err_with(|| format!("leyendo {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&raw).wrap_err_with(|| format!("parseando {}", path.display()))?;
        // Re-escribir siempre: agrega campos nuevos con defaults, preserva existentes
        cfg.save()?;
        Ok(cfg)
    }

    pub fn default() -> Result<Self> {
        let data_dir = dirs::data_dir()
            .ok_or_else(|| eyre!("no se pudo resolver el directorio de datos"))?
            .join("nubit-hosting-panel");

        Ok(Self {
            data_dir,
            caddyfile_path: PathBuf::from("/data/compose/1/conf/Caddyfile"),
            caddy_managed_path: PathBuf::from("/data/compose/1/conf/hostingctl.caddy"),
            caddy_validate_command: Some(
                "docker exec caddy caddy validate --config /etc/caddy/Caddyfile".to_string(),
            ),
            caddy_reload_command: Some(
                "docker exec caddy caddy reload --config /etc/caddy/Caddyfile".to_string(),
            ),
            mssql_sqlcmd_path: "/opt/mssql-tools18/bin/sqlcmd".to_string(),
            db_servers: HashMap::new(),
        })
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::default_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("hostingctl.sqlite3")
    }

    pub fn db_url(&self, server_name: &str) -> Option<String> {
        let env_name = format!("HOSTINGCTL_DB_{}_URL", env_key(server_name));
        std::env::var(env_name)
            .ok()
            .or_else(|| self.db_servers.get(server_name).map(|s| s.url.clone()))
    }
}

fn default_sqlcmd_path() -> String {
    "/opt/mssql-tools18/bin/sqlcmd".to_string()
}

fn env_key(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}
