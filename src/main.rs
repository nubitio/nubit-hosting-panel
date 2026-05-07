mod backup;
mod caddy;
mod config;
mod db;
mod docker;
mod doctor;
mod export;
mod mssql;
mod schedule;
mod ssh;
mod store;
mod tui;
mod update;
mod wp;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use color_eyre::eyre::{Result, bail};
use rand::distr::{Alphanumeric, SampleString};

use crate::{
    config::Config,
    store::{App as HostingApp, Client, DatabaseGrant, DomainAlias, SshUser, Store},
};

#[derive(Parser)]
#[command(name = "hostingctl", version, about = "Nubit Hosting Panel CLI/TUI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crea config local y base SQLite si no existen
    Init,
    /// Abre TUI básica
    Tui,
    /// Muestra resumen de estado local
    Status,
    /// Diagnostica dependencias, Caddy, Docker y DBs
    Doctor,
    /// Gestionar clientes
    Client(ClientCommand),
    /// Gestionar sitios/apps
    App(AppCommand),
    /// Gestionar usuarios y claves SSH
    Ssh(SshCommand),
    /// Gestionar Caddyfile
    Caddy(CaddyCommand),
    /// Gestionar servidores DB, bases, usuarios y grants
    Db(DbCommand),
    /// Provisionar WordPress en Docker Compose
    Wp(WpCommand),
    /// Exportar metadata del panel en JSON
    Export {
        #[arg(long, default_value = "hostingctl-export.json")]
        out: PathBuf,
    },
    /// Auto-actualizar hostingctl desde GitHub Releases
    Update {
        /// Solo verificar si hay actualización, sin instalar
        #[arg(long)]
        check: bool,
        /// Reinstalar aunque ya sea la versión más reciente
        #[arg(long)]
        force: bool,
    },
    /// Importar metadata del panel desde JSON
    Import {
        path: PathBuf,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args)]
struct ClientCommand {
    #[command(subcommand)]
    command: ClientSubcommand,
}

#[derive(Subcommand)]
enum ClientSubcommand {
    Add {
        slug: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        email: Option<String>,
    },
    Edit {
        slug: String,
        #[arg(long)]
        new_slug: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        notes: Option<String>,
    },
    Delete {
        slug: String,
        #[arg(long)]
        yes: bool,
    },
    List,
}

#[derive(Args)]
struct AppCommand {
    #[command(subcommand)]
    command: AppSubcommand,
}

#[derive(Subcommand)]
enum AppSubcommand {
    Add {
        client: String,
        slug: String,
        #[arg(long)]
        domain: String,
        #[arg(long)]
        upstream: String,
    },
    Edit {
        client: String,
        slug: String,
        #[arg(long)]
        new_client: Option<String>,
        #[arg(long)]
        new_slug: Option<String>,
        #[arg(long)]
        domain: String,
        #[arg(long)]
        upstream: String,
        #[arg(long)]
        notes: Option<String>,
    },
    Delete {
        client: String,
        slug: String,
        #[arg(long)]
        yes: bool,
    },
    AliasAdd {
        client: String,
        app: String,
        domain: String,
    },
    AliasList {
        client: Option<String>,
        app: Option<String>,
    },
    AliasDelete {
        domain: String,
        #[arg(long)]
        yes: bool,
    },
    Logs {
        client: String,
        slug: String,
        #[arg(long, default_value_t = 200)]
        tail: usize,
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        since: Option<String>,
    },
    Exec {
        client: String,
        slug: String,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    Shell {
        client: String,
        slug: String,
        #[arg(long, default_value = "auto")]
        shell: String,
    },
    List,
}

#[derive(Args)]
struct SshCommand {
    #[command(subcommand)]
    command: SshSubcommand,
}

#[derive(Subcommand)]
enum SshSubcommand {
    UserAdd {
        client: String,
        username: String,
        #[arg(long, default_value = "/bin/bash")]
        shell: String,
        #[arg(long)]
        home_dir: Option<PathBuf>,
        #[arg(long)]
        app: Option<String>,
    },
    UserList,
    UserEdit {
        username: String,
        #[arg(long)]
        client: String,
        #[arg(long, default_value = "/bin/bash")]
        shell: String,
        #[arg(long)]
        app: Option<String>,
    },
    UserDelete {
        username: String,
        #[arg(long)]
        yes: bool,
    },
    KeyAdd {
        username: String,
        label: String,
        public_key: String,
    },
    KeyList {
        username: Option<String>,
    },
    KeyDelete {
        username: String,
        label: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args)]
struct CaddyCommand {
    #[command(subcommand)]
    command: CaddySubcommand,
}

#[derive(Subcommand)]
enum CaddySubcommand {
    Bootstrap,
    Render,
    Apply {
        #[arg(long)]
        reload: bool,
    },
}

#[derive(Args)]
struct DbCommand {
    #[command(subcommand)]
    command: DbSubcommand,
}

#[derive(Args)]
struct WpCommand {
    #[command(subcommand)]
    command: WpSubcommand,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum WpSubcommand {
    /// Crea o migra un sitio WordPress con DB, compose y app proxy
    Provision {
        client: String,
        slug: String,
        #[arg(long)]
        domain: String,
        #[arg(long = "db-server")]
        db_server: String,
        #[arg(long, default_value = "fresh")]
        mode: String,
        #[arg(long)]
        bundle: Option<PathBuf>,
        #[arg(long)]
        archive: Option<PathBuf>,
        #[arg(long)]
        dump: Option<PathBuf>,
        #[arg(long)]
        reuse_existing_files: bool,
        #[arg(long)]
        old_domain: Option<String>,
        #[arg(long)]
        sites_dir: Option<PathBuf>,
        #[arg(long)]
        network: Option<String>,
        #[arg(long)]
        image: Option<String>,
        #[arg(long)]
        cli_image: Option<String>,
        #[arg(long, default_value = "%")]
        db_user_host: String,
        #[arg(long)]
        wp_db_host: Option<String>,
        #[arg(long = "no-apply-caddy", default_value_t = false)]
        no_apply_caddy: bool,
        #[arg(long = "no-reload-caddy", default_value_t = false)]
        no_reload_caddy: bool,
    },
    /// Aplica hardening de seguridad a un sitio WordPress existente
    Harden {
        client: String,
        slug: String,
        #[arg(long)]
        sites_dir: Option<PathBuf>,
        #[arg(long = "no-apply-caddy", default_value_t = false)]
        no_apply_caddy: bool,
        #[arg(long = "no-reload-caddy", default_value_t = false)]
        no_reload_caddy: bool,
    },
    /// Audita hardening y permisos de un sitio WordPress
    Audit {
        client: String,
        slug: String,
        #[arg(long)]
        sites_dir: Option<PathBuf>,
    },
    /// Escanea archivos WordPress buscando indicadores básicos de malware
    Scan {
        client: String,
        slug: String,
        #[arg(long)]
        sites_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum DbSubcommand {
    ServerAdd {
        name: String,
        #[arg(long, default_value = "mariadb")]
        kind: String,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 3306)]
        port: u16,
    },
    ServerList,
    CreateDatabase {
        server: String,
        client: String,
        name: String,
    },
    CreateUser {
        server: String,
        username: String,
        #[arg(long, default_value = "%")]
        host: String,
        #[arg(long, conflicts_with = "generate")]
        password: Option<String>,
        #[arg(long)]
        generate: bool,
    },
    Grant {
        server: String,
        client: String,
        database: String,
        username: String,
        #[arg(long, default_value = "%")]
        host: String,
    },
    GrantReassign {
        server: String,
        database: String,
        username: String,
        #[arg(long, default_value = "%")]
        host: String,
        #[arg(long)]
        client: String,
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        env: Option<String>,
    },
    Provision {
        server: String,
        client: String,
        app: String,
        #[arg(long, default_value = "prod")]
        env: String,
        #[arg(long, default_value = "%")]
        host: String,
        #[arg(long)]
        database: Option<String>,
        #[arg(long)]
        username: Option<String>,
        #[arg(long, conflicts_with = "generate")]
        password: Option<String>,
        #[arg(long)]
        generate: bool,
    },
    ResetPassword {
        server: String,
        username: String,
        #[arg(long, default_value = "%")]
        host: String,
        #[arg(long, conflicts_with = "generate")]
        password: Option<String>,
        #[arg(long)]
        generate: bool,
    },
    Backup {
        server: String,
        database: String,
        #[arg(long, default_value = "./backups")]
        out: PathBuf,
    },
    Restore {
        server: String,
        database: String,
        dump: PathBuf,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        dry_run: bool,
    },
    BackupList {
        #[arg(long, default_value = "./backups")]
        out: PathBuf,
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        database: Option<String>,
    },
    BackupAll {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    InstallTimer {
        #[arg(long, default_value = "daily")]
        schedule: String,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value = "/usr/local/bin/hostingctl")]
        binary: String,
    },
    TimerStatus,
    UninstallTimer,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let cfg = Config::load_or_create()?;
    let store = Store::open(&cfg.db_path())?;

    match cli.command {
        Command::Init => {
            println!("config: {}", Config::default_path()?.display());
            println!("data:   {}", cfg.data_dir.display());
            println!("db:     {}", cfg.db_path().display());
            println!("caddy:  {}", cfg.caddyfile_path.display());
            println!("managed:{}", cfg.caddy_managed_path.display());
        }
        Command::Tui => tui::run(&store, &cfg)?,
        Command::Status => print_status(&cfg, &store)?,
        Command::Doctor => {
            let checks = doctor::run(&cfg, &store)?;
            let failed = checks.iter().any(|check| !check.ok);
            doctor::print(&checks);
            if failed {
                std::process::exit(1);
            }
        }
        Command::Client(cmd) => match cmd.command {
            ClientSubcommand::Add { slug, name, email } => {
                let client = store.add_client(&slug, &name, email.as_deref())?;
                println!("cliente creado: {} ({})", client.slug, client.id);
            }
            ClientSubcommand::Edit {
                slug,
                new_slug,
                name,
                email,
                notes,
            } => {
                let client = find_client(&store, &slug)?;
                let next_slug = new_slug.as_deref().unwrap_or(&slug);
                store.update_client(
                    &client.id,
                    next_slug,
                    &name,
                    email.as_deref(),
                    notes.as_deref(),
                )?;
                println!("cliente actualizado: {}", next_slug);
            }
            ClientSubcommand::Delete { slug, yes } => {
                require_yes(yes, "delete client")?;
                let client = find_client(&store, &slug)?;
                store.delete_client(&client.id)?;
                println!("cliente eliminado: {}", slug);
            }
            ClientSubcommand::List => {
                for c in store.list_clients()? {
                    println!("{}\t{}\t{}", c.slug, c.name, c.email.unwrap_or_default());
                }
            }
        },
        Command::App(cmd) => match cmd.command {
            AppSubcommand::Add {
                client,
                slug,
                domain,
                upstream,
            } => {
                let app = store.add_app(&client, &slug, &domain, &upstream)?;
                println!(
                    "app creada: {}/{} {} -> {}",
                    app.client_slug, app.slug, app.domain, app.upstream
                );
            }
            AppSubcommand::List => {
                for a in store.list_apps()? {
                    println!("{}/{}\t{}\t{}", a.client_slug, a.slug, a.domain, a.upstream);
                }
            }
            AppSubcommand::Edit {
                client,
                slug,
                new_client,
                new_slug,
                domain,
                upstream,
                notes,
            } => {
                let app = find_app(&store, &client, &slug)?;
                let next_client = new_client.as_deref().unwrap_or(&client);
                let next_slug = new_slug.as_deref().unwrap_or(&slug);
                store.update_app(
                    &app.id,
                    next_client,
                    next_slug,
                    &domain,
                    &upstream,
                    notes.as_deref(),
                )?;
                println!("app actualizada: {}/{}", next_client, next_slug);
            }
            AppSubcommand::Delete { client, slug, yes } => {
                require_yes(yes, "delete app")?;
                let app = find_app(&store, &client, &slug)?;
                store.delete_app(&app.id)?;
                println!("app eliminada: {}/{}", client, slug);
            }
            AppSubcommand::AliasAdd {
                client,
                app,
                domain,
            } => {
                let app = find_app(&store, &client, &app)?;
                let alias = store.add_domain_alias(&app.id, &domain)?;
                println!(
                    "alias creado: {} -> {}/{}",
                    alias.domain, app.client_slug, app.slug
                );
            }
            AppSubcommand::AliasList { client, app } => {
                let aliases = store.list_domain_aliases()?;
                for alias in aliases {
                    let Some(parent) = store
                        .list_apps()?
                        .into_iter()
                        .find(|a| a.id == alias.app_id)
                    else {
                        continue;
                    };
                    if client.as_deref().is_some_and(|c| c != parent.client_slug) {
                        continue;
                    }
                    if app.as_deref().is_some_and(|s| s != parent.slug) {
                        continue;
                    }
                    println!("{}\t{}/{}", alias.domain, parent.client_slug, parent.slug);
                }
            }
            AppSubcommand::AliasDelete { domain, yes } => {
                require_yes(yes, "delete alias")?;
                let alias = find_alias_by_domain(&store, &domain)?;
                store.delete_domain_alias(&alias.id)?;
                println!("alias eliminado: {}", domain);
            }
            AppSubcommand::Logs {
                client,
                slug,
                tail,
                follow,
                since,
            } => {
                let app = find_app(&store, &client, &slug)?;
                let container = container_for_app(&app)?;
                docker::logs(&container, tail, follow, since.as_deref())?;
            }
            AppSubcommand::Exec {
                client,
                slug,
                command,
            } => {
                let app = find_app(&store, &client, &slug)?;
                let container = container_for_app(&app)?;
                docker::exec(&container, &command)?;
            }
            AppSubcommand::Shell {
                client,
                slug,
                shell,
            } => {
                let app = find_app(&store, &client, &slug)?;
                let container = container_for_app(&app)?;
                docker::shell(&container, &shell)?;
            }
        },
        Command::Ssh(cmd) => match cmd.command {
            SshSubcommand::UserAdd {
                client,
                username,
                shell,
                home_dir,
                app,
            } => {
                let home_dir = home_dir
                    .unwrap_or_else(|| PathBuf::from(format!("/home/{username}")))
                    .to_string_lossy()
                    .to_string();
                ssh::create_user(&username, &shell, &home_dir)?;
                match store.add_ssh_user(&username, &client, &shell, &home_dir, app.as_deref()) {
                    Ok(user) => println!("usuario SSH creado: {} ({})", user.username, user.id),
                    Err(e) => {
                        let _ = ssh::delete_user(&username);
                        return Err(e);
                    }
                }
            }
            SshSubcommand::UserList => {
                for user in store.list_ssh_users()? {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        user.client_slug,
                        user.app_slug.unwrap_or_default(),
                        user.username,
                        user.shell,
                        user.home_dir
                    );
                }
            }
            SshSubcommand::UserEdit {
                username,
                client,
                shell,
                app,
            } => {
                let user = find_ssh_user(&store, &username)?;
                if user.shell != shell {
                    ssh::set_shell(&username, &shell)?;
                }
                store.update_ssh_user(&user.id, &client, &shell, app.as_deref())?;
                println!("usuario SSH actualizado: {}", username);
            }
            SshSubcommand::UserDelete { username, yes } => {
                require_yes(yes, "delete ssh user")?;
                let user = find_ssh_user(&store, &username)?;
                store.delete_ssh_user(&user.id)?;
                ssh::delete_user(&username)?;
                println!("usuario SSH eliminado: {}", username);
            }
            SshSubcommand::KeyAdd {
                username,
                label,
                public_key,
            } => {
                let user = find_ssh_user(&store, &username)?;
                let key = store.add_ssh_key(&user.id, &label, &public_key)?;
                let keys = store.keys_for_user(&user.id)?;
                ssh::sync_authorized_keys(&username, &user.home_dir, &keys)?;
                println!("clave SSH creada: {} ({})", key.label, username);
            }
            SshSubcommand::KeyList { username } => {
                let keys = store.list_ssh_keys()?;
                let users = store.list_ssh_users()?;
                for key in keys {
                    let Some(user) = users.iter().find(|u| u.id == key.user_id) else {
                        continue;
                    };
                    if username
                        .as_deref()
                        .is_some_and(|name| name != user.username)
                    {
                        continue;
                    }
                    println!("{}\t{}\t{}", user.username, key.label, key.public_key);
                }
            }
            SshSubcommand::KeyDelete {
                username,
                label,
                yes,
            } => {
                require_yes(yes, "delete ssh key")?;
                let user = find_ssh_user(&store, &username)?;
                let key = store
                    .keys_for_user(&user.id)?
                    .into_iter()
                    .find(|key| key.label == label)
                    .ok_or_else(|| color_eyre::eyre::eyre!("clave no encontrada: {}", label))?;
                store.delete_ssh_key(&key.id)?;
                let keys = store.keys_for_user(&user.id)?;
                ssh::sync_authorized_keys(&username, &user.home_dir, &keys)?;
                println!("clave SSH eliminada: {} ({})", label, username);
            }
        },
        Command::Caddy(cmd) => match cmd.command {
            CaddySubcommand::Bootstrap => {
                let changed = caddy::bootstrap(&cfg)?;
                if changed {
                    println!("import agregado a {}", cfg.caddyfile_path.display());
                } else {
                    println!("import ya existía en {}", cfg.caddyfile_path.display());
                }
            }
            CaddySubcommand::Render => print!(
                "{}",
                caddy::render_block(&store.list_apps()?, &store.list_domain_aliases()?)
            ),
            CaddySubcommand::Apply { reload } => {
                caddy::apply(
                    &cfg,
                    &store.list_apps()?,
                    &store.list_domain_aliases()?,
                    reload,
                )?;
                println!(
                    "Caddy managed actualizado: {}",
                    cfg.caddy_managed_path.display()
                );
            }
        },
        Command::Export { out } => {
            let exported = export::write(&store, &out)?;
            println!(
                "export creado: {} (clients={}, apps={}, db_servers={}, grants={}, ssh_users={}, ssh_keys={}, aliases={})",
                out.display(),
                exported.clients.len(),
                exported.apps.len(),
                exported.db_servers.len(),
                exported.database_grants.len(),
                exported.ssh_users.len(),
                exported.ssh_keys.len(),
                exported.domain_aliases.len(),
            );
        }
        Command::Update { check, force } => {
            update::run(check, force)?;
        }
        Command::Import { path, dry_run, yes } => {
            let imported = export::read(&path)?;
            if dry_run {
                println!(
                    "dry-run OK: {} (version={}, clients={}, apps={}, db_servers={}, grants={}, ssh_users={}, ssh_keys={}, aliases={})",
                    path.display(),
                    imported.version,
                    imported.clients.len(),
                    imported.apps.len(),
                    imported.db_servers.len(),
                    imported.database_grants.len(),
                    imported.ssh_users.len(),
                    imported.ssh_keys.len(),
                    imported.domain_aliases.len(),
                );
            } else {
                if !yes {
                    bail!("import requiere --yes; usa --dry-run para validar sin aplicar");
                }
                let summary = export::import(&store, &imported)?;
                println!(
                    "import aplicado: clients={}, apps={}, db_servers={}, grants={}, ssh_users={}, ssh_keys={}, aliases={}",
                    summary.clients,
                    summary.apps,
                    summary.db_servers,
                    summary.database_grants,
                    summary.ssh_users,
                    summary.ssh_keys,
                    summary.domain_aliases,
                );
            }
        }
        Command::Db(cmd) => match cmd.command {
            DbSubcommand::ServerAdd {
                name,
                kind,
                host,
                port,
            } => {
                if kind != "mariadb" && kind != "mysql" && kind != "mssql" {
                    bail!("kind no soportado: {}; usa mariadb, mysql o mssql", kind);
                }
                let server = store.add_db_server(&name, &kind, &host, port)?;
                println!(
                    "db server creado: {} ({}) {}:{}",
                    server.name, server.kind, server.host, server.port
                );
                println!(
                    "credencial: define HOSTINGCTL_DB_{}_URL o [db_servers.{}].url",
                    server.name.to_ascii_uppercase().replace('-', "_"),
                    server.name
                );
            }
            DbSubcommand::ServerList => {
                for s in store.list_db_servers()? {
                    println!("{}\t{}\t{}:{}", s.name, s.kind, s.host, s.port);
                }
            }
            DbSubcommand::CreateDatabase {
                server,
                client: _,
                name,
            } => {
                let server = store.db_server(&server)?;
                db::create_database(&cfg, &server, &name)?;
                println!("database lista: {}", name);
            }
            DbSubcommand::CreateUser {
                server,
                username,
                host,
                password,
                generate,
            } => {
                let password = resolve_password(password, generate)?;
                let server = store.db_server(&server)?;
                db::create_user(&cfg, &server, &username, &host, &password)?;
                println!("usuario listo: '{}'@'{}'", username, host);
                print_secret(&password);
            }
            DbSubcommand::Grant {
                server,
                client,
                database,
                username,
                host,
            } => {
                let server = store.db_server(&server)?;
                db::grant_all(
                    &cfg, &store, &server, &client, None, "prod", &database, &username, &host,
                )?;
                println!(
                    "grant aplicado: {}.* -> '{}'@'{}'",
                    database, username, host
                );
            }
            DbSubcommand::GrantReassign {
                server,
                database,
                username,
                host,
                client,
                app,
                env,
            } => {
                let grant = find_database_grant(&store, &server, &database, &username, &host)?;
                store.reassign_database_grant(
                    &grant.id,
                    &client,
                    app.as_deref(),
                    env.as_deref(),
                )?;
                println!(
                    "grant reasignado: {} '{}'@'{}' -> client={} app={} env={}",
                    database,
                    username,
                    host,
                    client,
                    app.unwrap_or_else(|| "-".to_string()),
                    env.unwrap_or(grant.env)
                );
            }
            DbSubcommand::Provision {
                server,
                client,
                app,
                env,
                host,
                database,
                username,
                password,
                generate,
            } => {
                let password = resolve_password(password, generate)?;
                let server = store.db_server(&server)?;
                let provisioned = db::provision(
                    &cfg,
                    &store,
                    &server,
                    &client,
                    &app,
                    &env,
                    &host,
                    database.as_deref(),
                    username.as_deref(),
                    password,
                )?;
                println!("database lista: {}", provisioned.database);
                println!(
                    "usuario listo: '{}'@'{}'",
                    provisioned.username, provisioned.host
                );
                print_secret(&provisioned.password);
            }
            DbSubcommand::ResetPassword {
                server,
                username,
                host,
                password,
                generate,
            } => {
                let password = resolve_password(password, generate)?;
                let server = store.db_server(&server)?;
                db::reset_password(&cfg, &server, &username, &host, &password)?;
                println!("password actualizado: '{}'@'{}'", username, host);
                print_secret(&password);
            }
            DbSubcommand::Backup {
                server,
                database,
                out,
            } => {
                let server = store.db_server(&server)?;
                let path = backup::backup(&cfg, &server, &database, &out)?;
                println!("backup creado: {}", path.display());
            }
            DbSubcommand::Restore {
                server,
                database,
                dump,
                yes,
                dry_run,
            } => {
                let server = store.db_server(&server)?;
                if dry_run {
                    backup::dry_run_restore(&cfg, &server, &database, &dump)?;
                    println!("dry-run OK: {} <- {}", database, dump.display());
                } else {
                    if !yes {
                        bail!("restore requiere --yes; usa --dry-run para validar sin aplicar");
                    }
                    backup::restore(&cfg, &server, &database, &dump)?;
                    println!("restore aplicado: {} <- {}", database, dump.display());
                }
            }
            DbSubcommand::BackupList {
                out,
                server,
                database,
            } => {
                let files = backup::list_backups(&out, server.as_deref(), database.as_deref())?;
                for file in files {
                    println!("{}", file.display());
                }
            }
            DbSubcommand::BackupAll { out } => {
                let out_dir = out.unwrap_or_else(|| cfg.backup_dir.clone());
                let grants = store.list_database_grants()?;
                let mut seen = std::collections::HashSet::new();
                let mut ok = 0usize;
                let mut errs = 0usize;
                for grant in &grants {
                    let key = (grant.server_name.clone(), grant.db_name.clone());
                    if !seen.insert(key) {
                        continue;
                    }
                    match store.db_server(&grant.server_name) {
                        Err(e) => {
                            eprintln!("[skip] {} — {}", grant.server_name, e);
                            errs += 1;
                        }
                        Ok(server) => match backup::backup(&cfg, &server, &grant.db_name, &out_dir)
                        {
                            Ok(path) => {
                                println!("✓ {} — {}", grant.db_name, path.display());
                                ok += 1;
                            }
                            Err(e) => {
                                eprintln!("✗ {} — {}", grant.db_name, e);
                                errs += 1;
                            }
                        },
                    }
                }
                println!("\n{} backups OK, {} errores", ok, errs);
                if errs > 0 {
                    std::process::exit(1);
                }
            }
            DbSubcommand::InstallTimer {
                schedule,
                out,
                binary,
            } => {
                let out_dir = out.unwrap_or_else(|| cfg.backup_dir.clone());
                schedule::install(&schedule, &out_dir, &binary)?;
                println!("✓ timer instalado: hostingctl-backup.timer ({})", schedule);
                println!("  logs:   journalctl -u hostingctl-backup.service");
                println!("  status: hostingctl db timer-status");
            }
            DbSubcommand::TimerStatus => {
                let s = schedule::status()?;
                println!("service: {}", s.service_path);
                println!("timer:   {}", s.timer_path);
                println!("enabled: {}", s.enabled);
                println!(
                    "last:    {}",
                    s.last_run.unwrap_or_else(|| "never".to_string())
                );
                println!(
                    "next:    {}",
                    s.next_run.unwrap_or_else(|| "unknown".to_string())
                );
            }
            DbSubcommand::UninstallTimer => {
                schedule::uninstall()?;
                println!("✓ timer desinstalado");
            }
        },
        Command::Wp(cmd) => match cmd.command {
            WpSubcommand::Provision {
                client,
                slug,
                domain,
                db_server,
                mode,
                bundle,
                archive,
                dump,
                reuse_existing_files,
                old_domain,
                sites_dir,
                network,
                image,
                cli_image,
                db_user_host,
                wp_db_host,
                no_apply_caddy,
                no_reload_caddy,
            } => {
                let apply_caddy = !no_apply_caddy;
                let summary = wp::provision(
                    &cfg,
                    &store,
                    wp::ProvisionOptions {
                        client,
                        slug,
                        domain,
                        db_server,
                        mode: wp::ProvisionMode::parse(&mode)?,
                        bundle,
                        archive,
                        dump,
                        reuse_existing_files,
                        old_domain,
                        sites_dir: sites_dir.unwrap_or_else(|| cfg.wp_sites_dir.clone()),
                        network: network.unwrap_or_else(|| cfg.wp_network.clone()),
                        image: image.unwrap_or_else(|| cfg.wp_image.clone()),
                        cli_image: cli_image.unwrap_or_else(|| cfg.wp_cli_image.clone()),
                        db_user_host,
                        wp_db_host,
                        apply_caddy,
                        reload_caddy: apply_caddy && !no_reload_caddy,
                    },
                )?;
                println!("WordPress provisionado: {}", summary.site_dir.display());
                println!("compose:   {}", summary.compose_file.display());
                println!("html:      {}", summary.html_dir.display());
                println!("container: {}", summary.container);
                println!("upstream:  {}", summary.upstream);
                println!("database:  {}", summary.database);
                println!("db user:   {}", summary.username);
                print_secret(&summary.password);
                if !apply_caddy {
                    println!("siguiente: hostingctl caddy apply --reload");
                }
            }
            WpSubcommand::Harden {
                client,
                slug,
                sites_dir,
                no_apply_caddy,
                no_reload_caddy,
            } => {
                let summary = wp::harden_site(
                    &sites_dir.unwrap_or_else(|| cfg.wp_sites_dir.clone()),
                    &client,
                    &slug,
                )?;
                println!("WordPress harden aplicado: {}/{}", client, slug);
                println!("site:        {}", summary.site_dir.display());
                println!("html:        {}", summary.html_dir.display());
                println!(
                    "wp-config:   {}",
                    if summary.wp_config_hardened {
                        "updated"
                    } else {
                        "sin cambios/no encontrado"
                    }
                );
                println!(
                    "permisos:    {}",
                    if summary.permissions_normalized {
                        "normalizados"
                    } else {
                        "sin cambios"
                    }
                );
                if !no_apply_caddy {
                    caddy::apply(
                        &cfg,
                        &store.list_apps()?,
                        &store.list_domain_aliases()?,
                        !no_reload_caddy,
                    )?;
                    println!("caddy:       aplicado");
                }
            }
            WpSubcommand::Audit {
                client,
                slug,
                sites_dir,
            } => {
                let checks = wp::audit_site(
                    &sites_dir.unwrap_or_else(|| cfg.wp_sites_dir.clone()),
                    &client,
                    &slug,
                )?;
                for check in checks {
                    println!(
                        "{}\t{}\t{}",
                        check.status.as_str(),
                        check.name,
                        check.detail
                    );
                }
            }
            WpSubcommand::Scan {
                client,
                slug,
                sites_dir,
            } => {
                let findings = wp::scan_site(
                    &sites_dir.unwrap_or_else(|| cfg.wp_sites_dir.clone()),
                    &client,
                    &slug,
                )?;
                if findings.is_empty() {
                    println!("PASS\tscan\tsin indicadores básicos detectados");
                } else {
                    for finding in findings {
                        println!(
                            "{}\t{}\t{}",
                            finding.status.as_str(),
                            finding.path.display(),
                            finding.detail
                        );
                    }
                }
            }
        },
    }

    Ok(())
}

fn print_status(cfg: &Config, store: &Store) -> Result<()> {
    println!("Nubit Hosting Panel status");
    println!("config:  {}", Config::default_path()?.display());
    println!("data:    {}", cfg.data_dir.display());
    println!("sqlite:  {}", cfg.db_path().display());
    println!("caddy:   {}", cfg.caddyfile_path.display());
    println!("managed: {}", cfg.caddy_managed_path.display());
    println!();
    println!("clients:    {}", store.list_clients()?.len());
    println!("apps:       {}", store.list_apps()?.len());
    println!("db servers: {}", store.list_db_servers()?.len());
    println!("db grants:  {}", store.list_database_grants()?.len());
    Ok(())
}

fn resolve_password(password: Option<String>, generate: bool) -> Result<String> {
    match (password, generate) {
        (Some(password), false) => Ok(password),
        (None, true) => Ok(Alphanumeric.sample_string(&mut rand::rng(), 32)),
        _ => bail!("debes pasar --password 'secret' o --generate"),
    }
}

fn generate_password() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 32)
}

fn print_secret(password: &str) {
    println!("password (mostrar una sola vez): {}", password);
}

fn require_yes(yes: bool, action: &str) -> Result<()> {
    if !yes {
        bail!("{} requiere --yes", action);
    }
    Ok(())
}

fn find_client(store: &Store, slug: &str) -> Result<Client> {
    store
        .list_clients()?
        .into_iter()
        .find(|client| client.slug == slug)
        .ok_or_else(|| color_eyre::eyre::eyre!("cliente no encontrado: {}", slug))
}

fn find_app(store: &Store, client: &str, slug: &str) -> Result<HostingApp> {
    store
        .list_apps()?
        .into_iter()
        .find(|app| app.client_slug == client && app.slug == slug)
        .ok_or_else(|| color_eyre::eyre::eyre!("app no encontrada: {}/{}", client, slug))
}

fn container_for_app(app: &HostingApp) -> Result<String> {
    docker::container_name_from_upstream(&app.upstream).ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "app {}/{} no apunta a contenedor local: {}",
            app.client_slug,
            app.slug,
            app.upstream
        )
    })
}

fn find_alias_by_domain(store: &Store, domain: &str) -> Result<DomainAlias> {
    store
        .list_domain_aliases()?
        .into_iter()
        .find(|alias| alias.domain == domain)
        .ok_or_else(|| color_eyre::eyre::eyre!("alias no encontrado: {}", domain))
}

fn find_ssh_user(store: &Store, username: &str) -> Result<SshUser> {
    store
        .list_ssh_users()?
        .into_iter()
        .find(|user| user.username == username)
        .ok_or_else(|| color_eyre::eyre::eyre!("usuario SSH no encontrado: {}", username))
}

fn find_database_grant(
    store: &Store,
    server: &str,
    database: &str,
    username: &str,
    host: &str,
) -> Result<DatabaseGrant> {
    store
        .list_database_grants()?
        .into_iter()
        .find(|grant| {
            grant.server_name == server
                && grant.db_name == database
                && grant.username == username
                && grant.host == host
        })
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "grant no encontrado: server={} db={} user={} host={}",
                server,
                database,
                username,
                host
            )
        })
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_defines_ssh_and_alias_subcommands() {
        Cli::command().debug_assert();

        let root = Cli::command();
        assert!(root.get_subcommands().any(|cmd| cmd.get_name() == "ssh"));

        let app = root
            .get_subcommands()
            .find(|cmd| cmd.get_name() == "app")
            .expect("app command exists");
        assert!(
            app.get_subcommands()
                .any(|cmd| cmd.get_name() == "alias-add")
        );

        assert!(root.get_subcommands().any(|cmd| cmd.get_name() == "wp"));
    }

    #[test]
    fn cli_parses_wp_fresh_provision() {
        let cli = Cli::parse_from([
            "hostingctl",
            "wp",
            "provision",
            "client",
            "web",
            "--domain",
            "example.com",
            "--db-server",
            "mariadb",
        ]);

        match cli.command {
            Command::Wp(cmd) => match cmd.command {
                WpSubcommand::Provision {
                    client,
                    slug,
                    domain,
                    db_server,
                    mode,
                    ..
                } => {
                    assert_eq!(client, "client");
                    assert_eq!(slug, "web");
                    assert_eq!(domain, "example.com");
                    assert_eq!(db_server, "mariadb");
                    assert_eq!(mode, "fresh");
                }
                _ => panic!("expected wp provision"),
            },
            _ => panic!("expected wp provision"),
        }
    }

    #[test]
    fn cli_parses_wp_existing_bundle_retry_flags() {
        let cli = Cli::parse_from([
            "hostingctl",
            "wp",
            "provision",
            "client",
            "web",
            "--domain",
            "example.com",
            "--db-server",
            "mariadb",
            "--mode",
            "existing",
            "--bundle",
            "backup.tar.gz",
            "--reuse-existing-files",
            "--old-domain",
            "old.example.com",
            "--no-apply-caddy",
        ]);

        match cli.command {
            Command::Wp(cmd) => match cmd.command {
                WpSubcommand::Provision {
                    mode,
                    bundle,
                    reuse_existing_files,
                    old_domain,
                    no_apply_caddy,
                    ..
                } => {
                    assert_eq!(mode, "existing");
                    assert_eq!(
                        bundle.as_deref(),
                        Some(std::path::Path::new("backup.tar.gz"))
                    );
                    assert!(reuse_existing_files);
                    assert_eq!(old_domain.as_deref(), Some("old.example.com"));
                    assert!(no_apply_caddy);
                }
                _ => panic!("expected wp provision"),
            },
            _ => panic!("expected wp provision"),
        }
    }

    #[test]
    fn cli_parses_wp_harden() {
        let cli = Cli::parse_from([
            "hostingctl",
            "wp",
            "harden",
            "client",
            "web",
            "--no-reload-caddy",
        ]);

        match cli.command {
            Command::Wp(cmd) => match cmd.command {
                WpSubcommand::Harden {
                    client,
                    slug,
                    no_reload_caddy,
                    ..
                } => {
                    assert_eq!(client, "client");
                    assert_eq!(slug, "web");
                    assert!(no_reload_caddy);
                }
                _ => panic!("expected wp harden"),
            },
            _ => panic!("expected wp harden"),
        }
    }

    #[test]
    fn cli_parses_wp_audit_and_scan() {
        let audit = Cli::parse_from(["hostingctl", "wp", "audit", "client", "web"]);
        match audit.command {
            Command::Wp(cmd) => match cmd.command {
                WpSubcommand::Audit { client, slug, .. } => {
                    assert_eq!(client, "client");
                    assert_eq!(slug, "web");
                }
                _ => panic!("expected wp audit"),
            },
            _ => panic!("expected wp audit"),
        }

        let scan = Cli::parse_from(["hostingctl", "wp", "scan", "client", "web"]);
        match scan.command {
            Command::Wp(cmd) => match cmd.command {
                WpSubcommand::Scan { client, slug, .. } => {
                    assert_eq!(client, "client");
                    assert_eq!(slug, "web");
                }
                _ => panic!("expected wp scan"),
            },
            _ => panic!("expected wp scan"),
        }
    }
}
