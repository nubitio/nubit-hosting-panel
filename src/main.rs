mod backup;
mod caddy;
mod config;
mod db;
mod doctor;
mod export;
mod mssql;
mod schedule;
mod store;
mod tui;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use color_eyre::eyre::{Result, bail};
use rand::distr::{Alphanumeric, SampleString};

use crate::{config::Config, store::Store};

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
    /// Gestionar Caddyfile
    Caddy(CaddyCommand),
    /// Gestionar servidores DB, bases, usuarios y grants
    Db(DbCommand),
    /// Exportar metadata del panel en JSON
    Export {
        #[arg(long, default_value = "hostingctl-export.json")]
        out: PathBuf,
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
    List,
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
            CaddySubcommand::Render => print!("{}", caddy::render_block(&store.list_apps()?)),
            CaddySubcommand::Apply { reload } => {
                caddy::apply(&cfg, &store.list_apps()?, reload)?;
                println!(
                    "Caddy managed actualizado: {}",
                    cfg.caddy_managed_path.display()
                );
            }
        },
        Command::Export { out } => {
            let exported = export::write(&store, &out)?;
            println!(
                "export creado: {} (clients={}, apps={}, db_servers={}, grants={})",
                out.display(),
                exported.clients.len(),
                exported.apps.len(),
                exported.db_servers.len(),
                exported.database_grants.len()
            );
        }
        Command::Import { path, dry_run, yes } => {
            let imported = export::read(&path)?;
            if dry_run {
                println!(
                    "dry-run OK: {} (version={}, clients={}, apps={}, db_servers={}, grants={})",
                    path.display(),
                    imported.version,
                    imported.clients.len(),
                    imported.apps.len(),
                    imported.db_servers.len(),
                    imported.database_grants.len()
                );
            } else {
                if !yes {
                    bail!("import requiere --yes; usa --dry-run para validar sin aplicar");
                }
                let summary = export::import(&store, &imported)?;
                println!(
                    "import aplicado: clients={}, apps={}, db_servers={}, grants={}",
                    summary.clients, summary.apps, summary.db_servers, summary.database_grants
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

fn print_secret(password: &str) {
    println!("password (mostrar una sola vez): {}", password);
}
