use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

use crate::store::{App, Client, DatabaseGrant, DbServer, Store};

#[derive(Debug, Serialize, Deserialize)]
pub struct HostingExport {
    pub version: u32,
    pub exported_at: DateTime<Utc>,
    pub clients: Vec<Client>,
    pub apps: Vec<App>,
    pub db_servers: Vec<DbServer>,
    pub database_grants: Vec<DatabaseGrant>,
}

#[derive(Debug)]
pub struct ImportSummary {
    pub clients: usize,
    pub apps: usize,
    pub db_servers: usize,
    pub database_grants: usize,
}

pub fn build(store: &Store) -> Result<HostingExport> {
    Ok(HostingExport {
        version: 1,
        exported_at: Utc::now(),
        clients: store.list_clients()?,
        apps: store.list_apps()?,
        db_servers: store.list_db_servers()?,
        database_grants: store.list_database_grants()?,
    })
}

pub fn write(store: &Store, path: &Path) -> Result<HostingExport> {
    let export = build(store)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, serde_json::to_string_pretty(&export)?)?;
    Ok(export)
}

pub fn read(path: &Path) -> Result<HostingExport> {
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

pub fn import(store: &Store, export: &HostingExport) -> Result<ImportSummary> {
    for client in &export.clients {
        store.import_client(client)?;
    }
    for app in &export.apps {
        store.import_app(app)?;
    }
    for server in &export.db_servers {
        store.import_db_server(server)?;
    }
    for grant in &export.database_grants {
        store.import_database_grant(grant)?;
    }

    Ok(ImportSummary {
        clients: export.clients.len(),
        apps: export.apps.len(),
        db_servers: export.db_servers.len(),
        database_grants: export.database_grants.len(),
    })
}
