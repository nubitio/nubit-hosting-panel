use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

use crate::store::{App, Client, DatabaseGrant, DbServer, DomainAlias, SshKey, SshUser, Store};

#[derive(Debug, Serialize, Deserialize)]
pub struct HostingExport {
    pub version: u32,
    pub exported_at: DateTime<Utc>,
    pub clients: Vec<Client>,
    pub apps: Vec<App>,
    pub db_servers: Vec<DbServer>,
    pub database_grants: Vec<DatabaseGrant>,
    #[serde(default)]
    pub ssh_users: Vec<SshUser>,
    #[serde(default)]
    pub ssh_keys: Vec<SshKey>,
    #[serde(default)]
    pub domain_aliases: Vec<DomainAlias>,
}

#[derive(Debug)]
pub struct ImportSummary {
    pub clients: usize,
    pub apps: usize,
    pub db_servers: usize,
    pub database_grants: usize,
    pub ssh_users: usize,
    pub ssh_keys: usize,
    pub domain_aliases: usize,
}

pub fn build(store: &Store) -> Result<HostingExport> {
    Ok(HostingExport {
        version: 2,
        exported_at: Utc::now(),
        clients: store.list_clients()?,
        apps: store.list_apps()?,
        db_servers: store.list_db_servers()?,
        database_grants: store.list_database_grants()?,
        ssh_users: store.list_ssh_users()?,
        ssh_keys: store.list_ssh_keys()?,
        domain_aliases: store.list_domain_aliases()?,
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
    for user in &export.ssh_users {
        store.import_ssh_user(user)?;
    }
    for key in &export.ssh_keys {
        store.import_ssh_key(key)?;
    }
    for alias in &export.domain_aliases {
        store.import_domain_alias(alias)?;
    }

    Ok(ImportSummary {
        clients: export.clients.len(),
        apps: export.apps.len(),
        db_servers: export.db_servers.len(),
        database_grants: export.database_grants.len(),
        ssh_users: export.ssh_users.len(),
        ssh_keys: export.ssh_keys.len(),
        domain_aliases: export.domain_aliases.len(),
    })
}
