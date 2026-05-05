use chrono::{DateTime, Utc};
use color_eyre::eyre::{Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainAlias {
    pub id: String,
    pub app_id: String,
    pub domain: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Client {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub email: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct App {
    pub id: String,
    pub client_slug: String,
    pub slug: String,
    pub domain: String,
    pub upstream: String,
    #[serde(default)]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbServer {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshUser {
    pub id: String,
    pub client_slug: String,
    pub app_slug: Option<String>,
    pub username: String,
    pub shell: String,
    pub home_dir: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshKey {
    pub id: String,
    pub user_id: String,
    pub label: String,
    pub public_key: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseGrant {
    pub id: String,
    pub server_name: String,
    pub client_slug: String,
    pub app_slug: Option<String>,
    pub env: String,
    pub db_name: String,
    pub username: String,
    pub host: String,
    pub created_at: DateTime<Utc>,
}

pub struct Store {
    conn: Connection,
}

/// Cada entrada es una migración numerada desde 1.
/// Para agregar schema nuevo:
///   1. push una nueva entrada a este array
///   2. el binario aplicará automáticamente solo las migraciones pendientes
/// NO modificar migraciones existentes.
const MIGRATIONS: &[&str] = &[
    // v1 — schema inicial
    r#"
    CREATE TABLE IF NOT EXISTS clients (
        id TEXT PRIMARY KEY,
        slug TEXT NOT NULL UNIQUE,
        name TEXT NOT NULL,
        email TEXT,
        created_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS apps (
        id TEXT PRIMARY KEY,
        client_id TEXT NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
        slug TEXT NOT NULL,
        domain TEXT NOT NULL UNIQUE,
        upstream TEXT NOT NULL,
        created_at TEXT NOT NULL,
        UNIQUE(client_id, slug)
    );

    CREATE TABLE IF NOT EXISTS db_servers (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        kind TEXT NOT NULL,
        host TEXT NOT NULL,
        port INTEGER NOT NULL,
        created_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS database_grants (
        id TEXT PRIMARY KEY,
        server_id TEXT NOT NULL REFERENCES db_servers(id) ON DELETE CASCADE,
        client_id TEXT NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
        app_id TEXT REFERENCES apps(id) ON DELETE SET NULL,
        env TEXT NOT NULL DEFAULT 'prod',
        db_name TEXT NOT NULL,
        username TEXT NOT NULL,
        host TEXT NOT NULL,
        created_at TEXT NOT NULL,
        UNIQUE(server_id, db_name, username, host)
    );
    "#,
    // v2 — usuarios y claves SSH
    r#"
    CREATE TABLE IF NOT EXISTS ssh_users (
        id TEXT PRIMARY KEY,
        client_id TEXT NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
        username TEXT NOT NULL UNIQUE,
        shell TEXT NOT NULL DEFAULT '/bin/bash',
        home_dir TEXT NOT NULL,
        created_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS ssh_keys (
        id TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES ssh_users(id) ON DELETE CASCADE,
        label TEXT NOT NULL,
        public_key TEXT NOT NULL,
        created_at TEXT NOT NULL,
        UNIQUE(user_id, label)
    );
    "#,
    // v3 — asociación opcional SSH user → app
    r#"ALTER TABLE ssh_users ADD COLUMN app_id TEXT REFERENCES apps(id) ON DELETE SET NULL;"#,
    // v4 — dominios alias por app
    r#"
    CREATE TABLE IF NOT EXISTS domain_aliases (
        id TEXT PRIMARY KEY,
        app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
        domain TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL
    );
    "#,
    // v5 — notas opcionales en clientes y apps
    r#"
    ALTER TABLE clients ADD COLUMN notes TEXT;
    ALTER TABLE apps ADD COLUMN notes TEXT;
    "#,
];

impl Store {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;

        for (i, migration) in MIGRATIONS.iter().enumerate() {
            let target = (i + 1) as i64;
            if version < target {
                self.conn.execute_batch(migration)?;
                self.conn
                    .execute_batch(&format!("PRAGMA user_version = {target}"))?;
            }
        }
        Ok(())
    }

    pub fn add_client(&self, slug: &str, name: &str, email: Option<&str>) -> Result<Client> {
        validate_slug(slug)?;
        let client = Client {
            id: Uuid::new_v4().to_string(),
            slug: slug.to_string(),
            name: name.to_string(),
            email: email.map(str::to_string),
            notes: None,
            created_at: Utc::now(),
        };
        self.conn.execute(
            "INSERT INTO clients (id, slug, name, email, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                client.id,
                client.slug,
                client.name,
                client.email,
                client.created_at.to_rfc3339()
            ],
        )?;
        Ok(client)
    }

    pub fn list_clients(&self) -> Result<Vec<Client>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, slug, name, email, notes, created_at FROM clients ORDER BY slug")?;
        let rows = stmt.query_map([], |row| {
            Ok(Client {
                id: row.get(0)?,
                slug: row.get(1)?,
                name: row.get(2)?,
                email: row.get(3)?,
                notes: row.get(4)?,
                created_at: parse_dt(row.get::<_, String>(5)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn client_id(&self, slug: &str) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT id FROM clients WHERE slug = ?1", [slug], |r| {
                r.get(0)
            })?)
    }

    pub fn app_id(&self, client_slug: &str, app_slug: &str) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT a.id FROM apps a JOIN clients c ON c.id = a.client_id WHERE c.slug = ?1 AND a.slug = ?2",
            params![client_slug, app_slug],
            |r| r.get(0),
        )?)
    }

    pub fn add_app(
        &self,
        client_slug: &str,
        slug: &str,
        domain: &str,
        upstream: &str,
    ) -> Result<App> {
        validate_slug(client_slug)?;
        validate_slug(slug)?;
        let client_id = self.client_id(client_slug)?;
        let app = App {
            id: Uuid::new_v4().to_string(),
            client_slug: client_slug.to_string(),
            slug: slug.to_string(),
            domain: domain.to_string(),
            upstream: upstream.to_string(),
            notes: None,
            created_at: Utc::now(),
        };
        self.conn.execute(
            "INSERT INTO apps (id, client_id, slug, domain, upstream, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![app.id, client_id, app.slug, app.domain, app.upstream, app.created_at.to_rfc3339()],
        )?;
        Ok(app)
    }

    pub fn list_apps(&self) -> Result<Vec<App>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, c.slug, a.slug, a.domain, a.upstream, a.notes, a.created_at FROM apps a JOIN clients c ON c.id = a.client_id ORDER BY c.slug, a.slug",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(App {
                id: row.get(0)?,
                client_slug: row.get(1)?,
                slug: row.get(2)?,
                domain: row.get(3)?,
                upstream: row.get(4)?,
                notes: row.get(5)?,
                created_at: parse_dt(row.get::<_, String>(6)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn add_db_server(&self, name: &str, kind: &str, host: &str, port: u16) -> Result<DbServer> {
        validate_slug(name)?;
        let server = DbServer {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            kind: kind.to_string(),
            host: host.to_string(),
            port,
        };
        self.conn.execute(
            "INSERT INTO db_servers (id, name, kind, host, port, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![server.id, server.name, server.kind, server.host, server.port, Utc::now().to_rfc3339()],
        )?;
        Ok(server)
    }

    pub fn db_server(&self, name: &str) -> Result<DbServer> {
        Ok(self.conn.query_row(
            "SELECT id, name, kind, host, port FROM db_servers WHERE name = ?1",
            [name],
            |row| {
                Ok(DbServer {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    host: row.get(3)?,
                    port: row.get(4)?,
                })
            },
        )?)
    }

    pub fn list_db_servers(&self) -> Result<Vec<DbServer>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, kind, host, port FROM db_servers ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok(DbServer {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                host: row.get(3)?,
                port: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_database_grants(&self) -> Result<Vec<DatabaseGrant>> {
        let mut stmt = self.conn.prepare(
            "SELECT g.id, s.name, c.slug, a.slug, g.env, g.db_name, g.username, g.host, g.created_at
             FROM database_grants g
             JOIN db_servers s ON s.id = g.server_id
             JOIN clients c ON c.id = g.client_id
             LEFT JOIN apps a ON a.id = g.app_id
             ORDER BY s.name, c.slug, a.slug, g.env, g.db_name, g.username",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DatabaseGrant {
                id: row.get(0)?,
                server_name: row.get(1)?,
                client_slug: row.get(2)?,
                app_slug: row.get(3)?,
                env: row.get(4)?,
                db_name: row.get(5)?,
                username: row.get(6)?,
                host: row.get(7)?,
                created_at: parse_dt(row.get::<_, String>(8)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn import_client(&self, client: &Client) -> Result<()> {
        validate_slug(&client.slug)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO clients (id, slug, name, email, notes, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![client.id, client.slug, client.name, client.email, client.notes, client.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn import_app(&self, app: &App) -> Result<()> {
        validate_slug(&app.client_slug)?;
        validate_slug(&app.slug)?;
        let client_id = self.client_id(&app.client_slug)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO apps (id, client_id, slug, domain, upstream, notes, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![app.id, client_id, app.slug, app.domain, app.upstream, app.notes, app.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn import_ssh_user(&self, user: &SshUser) -> Result<()> {
        let client_id = self.client_id(&user.client_slug)?;
        let app_id = match &user.app_slug {
            Some(a) => Some(self.app_id(&user.client_slug, a)?),
            None => None,
        };
        self.conn.execute(
            "INSERT OR IGNORE INTO ssh_users (id, client_id, app_id, username, shell, home_dir, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![user.id, client_id, app_id, user.username, user.shell, user.home_dir, user.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn import_ssh_key(&self, key: &SshKey) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO ssh_keys (id, user_id, label, public_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![key.id, key.user_id, key.label, key.public_key, key.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn import_domain_alias(&self, alias: &DomainAlias) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO domain_aliases (id, app_id, domain, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![alias.id, alias.app_id, alias.domain, alias.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn import_db_server(&self, server: &DbServer) -> Result<()> {
        validate_slug(&server.name)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO db_servers (id, name, kind, host, port, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![server.id, server.name, server.kind, server.host, server.port, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn import_database_grant(&self, grant: &DatabaseGrant) -> Result<()> {
        let server = self.db_server(&grant.server_name)?;
        self.record_grant(
            &server.id,
            &grant.client_slug,
            grant.app_slug.as_deref(),
            &grant.env,
            &grant.db_name,
            &grant.username,
            &grant.host,
        )
    }

    pub fn record_grant(
        &self,
        server_id: &str,
        client_slug: &str,
        app_slug: Option<&str>,
        env: &str,
        db_name: &str,
        username: &str,
        host: &str,
    ) -> Result<()> {
        let client_id = self.client_id(client_slug)?;
        let app_id = match app_slug {
            Some(app) => Some(self.app_id(client_slug, app)?),
            None => None,
        };
        self.conn.execute(
            "INSERT OR IGNORE INTO database_grants (id, server_id, client_id, app_id, env, db_name, username, host, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![Uuid::new_v4().to_string(), server_id, client_id, app_id, env, db_name, username, host, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn update_client(
        &self,
        id: &str,
        slug: &str,
        name: &str,
        email: Option<&str>,
        notes: Option<&str>,
    ) -> Result<()> {
        validate_slug(slug)?;
        let changed = self.conn.execute(
            "UPDATE clients SET slug = ?1, name = ?2, email = ?3, notes = ?4 WHERE id = ?5",
            params![slug, name, email, notes, id],
        )?;
        if changed == 0 {
            bail!("cliente no encontrado: {}", id);
        }
        Ok(())
    }

    pub fn update_app(
        &self,
        id: &str,
        client_slug: &str,
        slug: &str,
        domain: &str,
        upstream: &str,
        notes: Option<&str>,
    ) -> Result<()> {
        validate_slug(client_slug)?;
        validate_slug(slug)?;
        let client_id = self.client_id(client_slug)?;
        let changed = self.conn.execute(
            "UPDATE apps SET client_id = ?1, slug = ?2, domain = ?3, upstream = ?4, notes = ?5 WHERE id = ?6",
            params![client_id, slug, domain, upstream, notes, id],
        )?;
        if changed == 0 {
            bail!("app no encontrada: {}", id);
        }
        Ok(())
    }

    pub fn add_ssh_user(
        &self,
        username: &str,
        client_slug: &str,
        shell: &str,
        home_dir: &str,
        app_slug: Option<&str>,
    ) -> Result<SshUser> {
        validate_slug(username)?;
        let client_id = self.client_id(client_slug)?;
        let app_id = match app_slug {
            Some(a) => Some(self.app_id(client_slug, a)?),
            None => None,
        };
        let user = SshUser {
            id: Uuid::new_v4().to_string(),
            client_slug: client_slug.to_string(),
            app_slug: app_slug.map(str::to_string),
            username: username.to_string(),
            shell: shell.to_string(),
            home_dir: home_dir.to_string(),
            created_at: Utc::now(),
        };
        self.conn.execute(
            "INSERT INTO ssh_users (id, client_id, app_id, username, shell, home_dir, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![user.id, client_id, app_id, user.username, user.shell, user.home_dir, user.created_at.to_rfc3339()],
        )?;
        Ok(user)
    }

    pub fn list_ssh_users(&self) -> Result<Vec<SshUser>> {
        let mut stmt = self.conn.prepare(
            "SELECT u.id, c.slug, a.slug, u.username, u.shell, u.home_dir, u.created_at
             FROM ssh_users u
             JOIN clients c ON c.id = u.client_id
             LEFT JOIN apps a ON a.id = u.app_id
             ORDER BY c.slug, u.username",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SshUser {
                id: row.get(0)?,
                client_slug: row.get(1)?,
                app_slug: row.get(2)?,
                username: row.get(3)?,
                shell: row.get(4)?,
                home_dir: row.get(5)?,
                created_at: parse_dt(row.get::<_, String>(6)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_ssh_user(
        &self,
        id: &str,
        client_slug: &str,
        shell: &str,
        app_slug: Option<&str>,
    ) -> Result<()> {
        let client_id = self.client_id(client_slug)?;
        let app_id = match app_slug {
            Some(a) => Some(self.app_id(client_slug, a)?),
            None => None,
        };
        let changed = self.conn.execute(
            "UPDATE ssh_users SET client_id = ?1, shell = ?2, app_id = ?3 WHERE id = ?4",
            params![client_id, shell, app_id, id],
        )?;
        if changed == 0 {
            bail!("usuario SSH no encontrado: {}", id);
        }
        Ok(())
    }

    pub fn delete_ssh_user(&self, id: &str) -> Result<()> {
        let changed = self.conn.execute("DELETE FROM ssh_users WHERE id = ?1", [id])?;
        if changed == 0 {
            bail!("usuario SSH no encontrado: {}", id);
        }
        Ok(())
    }

    pub fn add_ssh_key(&self, user_id: &str, label: &str, public_key: &str) -> Result<SshKey> {
        let key = SshKey {
            id: Uuid::new_v4().to_string(),
            user_id: user_id.to_string(),
            label: label.to_string(),
            public_key: public_key.to_string(),
            created_at: Utc::now(),
        };
        self.conn.execute(
            "INSERT INTO ssh_keys (id, user_id, label, public_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![key.id, key.user_id, key.label, key.public_key, key.created_at.to_rfc3339()],
        )?;
        Ok(key)
    }

    pub fn list_ssh_keys(&self) -> Result<Vec<SshKey>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, label, public_key, created_at FROM ssh_keys ORDER BY user_id, created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SshKey {
                id: row.get(0)?,
                user_id: row.get(1)?,
                label: row.get(2)?,
                public_key: row.get(3)?,
                created_at: parse_dt(row.get::<_, String>(4)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn keys_for_user(&self, user_id: &str) -> Result<Vec<SshKey>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, label, public_key, created_at FROM ssh_keys WHERE user_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([user_id], |row| {
            Ok(SshKey {
                id: row.get(0)?,
                user_id: row.get(1)?,
                label: row.get(2)?,
                public_key: row.get(3)?,
                created_at: parse_dt(row.get::<_, String>(4)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete_ssh_key(&self, id: &str) -> Result<()> {
        let changed = self.conn.execute("DELETE FROM ssh_keys WHERE id = ?1", [id])?;
        if changed == 0 {
            bail!("clave SSH no encontrada: {}", id);
        }
        Ok(())
    }

    pub fn add_domain_alias(&self, app_id: &str, domain: &str) -> Result<DomainAlias> {
        if domain.trim().is_empty() {
            bail!("el dominio no puede estar vacío");
        }
        let alias = DomainAlias {
            id: Uuid::new_v4().to_string(),
            app_id: app_id.to_string(),
            domain: domain.trim().to_string(),
            created_at: Utc::now(),
        };
        self.conn.execute(
            "INSERT INTO domain_aliases (id, app_id, domain, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![alias.id, alias.app_id, alias.domain, alias.created_at.to_rfc3339()],
        )?;
        Ok(alias)
    }

    pub fn list_domain_aliases(&self) -> Result<Vec<DomainAlias>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, app_id, domain, created_at FROM domain_aliases ORDER BY app_id, domain",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DomainAlias {
                id: row.get(0)?,
                app_id: row.get(1)?,
                domain: row.get(2)?,
                created_at: parse_dt(row.get::<_, String>(3)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn aliases_for_app(&self, app_id: &str) -> Result<Vec<DomainAlias>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, app_id, domain, created_at FROM domain_aliases WHERE app_id = ?1 ORDER BY domain",
        )?;
        let rows = stmt.query_map([app_id], |row| {
            Ok(DomainAlias {
                id: row.get(0)?,
                app_id: row.get(1)?,
                domain: row.get(2)?,
                created_at: parse_dt(row.get::<_, String>(3)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete_domain_alias(&self, id: &str) -> Result<()> {
        let changed = self
            .conn
            .execute("DELETE FROM domain_aliases WHERE id = ?1", [id])?;
        if changed == 0 {
            bail!("alias no encontrado: {}", id);
        }
        Ok(())
    }

    pub fn delete_client(&self, id: &str) -> Result<()> {
        let changed = self
            .conn
            .execute("DELETE FROM clients WHERE id = ?1", [id])?;
        if changed == 0 {
            bail!("cliente no encontrado: {}", id);
        }
        Ok(())
    }

    pub fn delete_app(&self, id: &str) -> Result<()> {
        let changed = self.conn.execute("DELETE FROM apps WHERE id = ?1", [id])?;
        if changed == 0 {
            bail!("app no encontrada: {}", id);
        }
        Ok(())
    }
}

pub fn validate_slug(value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        bail!("slug inválido `{}`; usa solo a-z, 0-9, - y _", value);
    }
    Ok(())
}

pub fn ensure_identifier(value: &str) -> Result<()> {
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

fn parse_dt(raw: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_slug_accepts_safe_values() {
        assert!(validate_slug("porteroseguro").is_ok());
        assert!(validate_slug("cliente-123").is_ok());
        assert!(validate_slug("cliente_123").is_ok());
    }

    #[test]
    fn validate_slug_rejects_unsafe_values() {
        assert!(validate_slug("").is_err());
        assert!(validate_slug("PorteroSeguro").is_err());
        assert!(validate_slug("portero seguro").is_err());
        assert!(validate_slug("../x").is_err());
    }

    #[test]
    fn migrations_set_user_version() {
        let tmp = tempfile();
        let store = Store::open(&tmp).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
    }

    #[test]
    fn migrations_are_idempotent() {
        let tmp = tempfile();
        Store::open(&tmp).unwrap();
        // opening again must not fail
        Store::open(&tmp).unwrap();
    }

    fn tempfile() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("hostingctl-test-{}.sqlite3", uuid::Uuid::new_v4()))
    }
}
