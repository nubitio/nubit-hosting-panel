use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use color_eyre::eyre::{Context, Result, bail};

use crate::{
    backup, caddy,
    config::Config,
    db, docker,
    store::{DbServer, ProvisionedDb, Store, convention_names, validate_slug},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionMode {
    Fresh,
    Existing,
}

impl ProvisionMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "fresh" => Ok(Self::Fresh),
            "existing" => Ok(Self::Existing),
            _ => bail!("mode inválido `{}`; usa fresh o existing", raw),
        }
    }
}

#[derive(Debug)]
pub struct ProvisionOptions {
    pub client: String,
    pub slug: String,
    pub domain: String,
    pub db_server: String,
    pub mode: ProvisionMode,
    pub bundle: Option<PathBuf>,
    pub archive: Option<PathBuf>,
    pub dump: Option<PathBuf>,
    pub reuse_existing_files: bool,
    pub old_domain: Option<String>,
    pub sites_dir: PathBuf,
    pub network: String,
    pub image: String,
    pub cli_image: String,
    pub db_user_host: String,
    pub wp_db_host: Option<String>,
    pub apply_caddy: bool,
    pub reload_caddy: bool,
}

#[derive(Debug)]
pub struct ProvisionSummary {
    pub site_dir: PathBuf,
    pub html_dir: PathBuf,
    pub compose_file: PathBuf,
    pub container: String,
    pub upstream: String,
    pub database: String,
    pub username: String,
    pub password: String,
}

pub fn provision(cfg: &Config, store: &Store, opts: ProvisionOptions) -> Result<ProvisionSummary> {
    validate_slug(&opts.client)?;
    validate_slug(&opts.slug)?;
    validate_slug(&opts.network)?;
    validate_existing_inputs(&opts)?;

    let server = store.db_server(&opts.db_server)?;
    let container = wordpress_container_name(&opts.client, &opts.slug);
    let upstream = format!("{container}:80");
    let site_dir = opts.sites_dir.join(&opts.client).join(&opts.slug);
    let html_dir = site_dir.join("html");
    let imports_dir = site_dir.join("imports");
    let compose_file = site_dir.join("compose.yml");
    let env_file = site_dir.join(".env");
    let wp_db_host = opts
        .wp_db_host
        .clone()
        .unwrap_or_else(|| format!("{}:{}", server.name, server.port));

    fs::create_dir_all(&html_dir).wrap_err_with(|| format!("creando {}", html_dir.display()))?;
    fs::create_dir_all(&imports_dir)
        .wrap_err_with(|| format!("creando {}", imports_dir.display()))?;

    let bundle = match &opts.bundle {
        Some(bundle) => Some(prepare_bundle(bundle, &imports_dir, &html_dir)?),
        None => None,
    };

    if let Some(archive) = &opts.archive {
        extract_archive_to_dir(archive, &html_dir)?;
        flatten_single_wp_root(&html_dir)?;
    }

    if opts.mode == ProvisionMode::Existing && (opts.bundle.is_some() || opts.archive.is_some()) {
        backup_migrated_wp_config(&html_dir)?;
    }

    let dump_path = opts
        .dump
        .as_deref()
        .or_else(|| bundle.as_ref().and_then(|bundle| bundle.dump.as_deref()));

    let provisioned = provision_database_without_app_link(
        cfg,
        store,
        &server,
        &opts.client,
        &opts.slug,
        &opts.db_user_host,
    )
    .wrap_err("provisionando database WordPress")?;

    write_env_file(
        &env_file,
        &provisioned.database,
        &provisioned.username,
        &provisioned.password,
        &wp_db_host,
    )?;
    write_compose_file(&compose_file, &opts, &container)?;

    if let Some(dump) = dump_path {
        eprintln!("[hostingctl] importando dump: {}", dump.display());
        backup::restore(cfg, &server, &provisioned.database, dump)
            .wrap_err_with(|| format!("importando dump {}", dump.display()))?;
        eprintln!("[hostingctl] dump importado OK");
    }

    docker::ensure_network(&opts.network)?;
    docker::compose_pull(&site_dir)?;
    docker::compose_up(&site_dir)?;

    ensure_app_registered(store, &opts.client, &opts.slug, &opts.domain, &upstream)?;
    link_grant_to_app(
        store,
        &server.name,
        &provisioned.database,
        &provisioned.username,
        &provisioned.host,
        &opts.client,
        &opts.slug,
    )?;

    if let Some(old_domain) = &opts.old_domain {
        run_search_replace(&site_dir, &opts, old_domain, &opts.domain)?;
    }

    if opts.apply_caddy {
        caddy::apply(
            cfg,
            &store.list_apps()?,
            &store.list_domain_aliases()?,
            opts.reload_caddy,
        )?;
    }

    Ok(ProvisionSummary {
        site_dir,
        html_dir,
        compose_file,
        container,
        upstream,
        database: provisioned.database,
        username: provisioned.username,
        password: provisioned.password,
    })
}

fn provision_database_without_app_link(
    cfg: &Config,
    store: &Store,
    server: &DbServer,
    client: &str,
    app: &str,
    host: &str,
) -> Result<ProvisionedDb> {
    if server.kind != "mariadb" && server.kind != "mysql" {
        bail!(
            "WordPress requiere DB mariadb/mysql; server `{}` es `{}`",
            server.name,
            server.kind
        );
    }
    let (database, username) = convention_names(client, app, "prod")?;
    let password = crate::generate_password();

    db::create_database(cfg, server, &database)?;
    db::create_user(cfg, server, &username, host, &password)?;
    db::grant_all(
        cfg, store, server, client, None, "prod", &database, &username, host,
    )?;
    db::reset_password(cfg, server, &username, host, &password)
        .wrap_err("sincronizando password database WordPress")?;

    Ok(ProvisionedDb {
        database,
        username,
        host: host.to_string(),
        password,
    })
}

pub fn wordpress_container_name(client: &str, slug: &str) -> String {
    format!(
        "{}_{}_wordpress",
        container_part(client),
        container_part(slug)
    )
}

fn container_part(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn validate_existing_inputs(opts: &ProvisionOptions) -> Result<()> {
    if opts.mode == ProvisionMode::Fresh
        && (opts.bundle.is_some() || opts.archive.is_some() || opts.dump.is_some())
    {
        bail!("mode fresh no acepta --bundle, --archive ni --dump; usa --mode existing");
    }
    if opts.mode == ProvisionMode::Existing
        && opts.bundle.is_none()
        && opts.archive.is_none()
        && opts.dump.is_none()
        && !opts.reuse_existing_files
    {
        bail!("mode existing requiere --bundle, --archive, --dump o --reuse-existing-files");
    }
    Ok(())
}

fn write_env_file(
    path: &Path,
    database: &str,
    username: &str,
    password: &str,
    wp_db_host: &str,
) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .wrap_err_with(|| format!("creando {}", path.display()))?;

    writeln!(file, "WORDPRESS_DB_HOST={wp_db_host}")?;
    writeln!(file, "WORDPRESS_DB_NAME={database}")?;
    writeln!(file, "WORDPRESS_DB_USER={username}")?;
    writeln!(file, "WORDPRESS_DB_PASSWORD={password}")?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .wrap_err_with(|| format!("ajustando permisos de {}", path.display()))?;
    Ok(())
}

fn write_compose_file(path: &Path, opts: &ProvisionOptions, container: &str) -> Result<()> {
    let compose = render_compose(&opts.image, container, &opts.network);
    fs::write(path, compose).wrap_err_with(|| format!("escribiendo {}", path.display()))?;
    Ok(())
}

pub fn render_compose(image: &str, container: &str, network: &str) -> String {
    format!(
        r#"services:
  wordpress:
    image: {image}
    container_name: {container}
    restart: unless-stopped
    env_file:
      - .env
    volumes:
      - ./html:/var/www/html
    networks:
      - {network}

networks:
  {network}:
    external: true
"#
    )
}

#[derive(Debug)]
struct PreparedBundle {
    dump: Option<PathBuf>,
}

fn prepare_bundle(bundle: &Path, imports_dir: &Path, html_dir: &Path) -> Result<PreparedBundle> {
    let staging = imports_dir.join(format!(
        "bundle-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    ));
    fs::create_dir_all(&staging).wrap_err_with(|| format!("creando {}", staging.display()))?;
    extract_archive_to_dir(bundle, &staging)?;

    if let Some(root) = find_wp_root(&staging)? {
        copy_dir_contents(&root, html_dir)?;
        flatten_single_wp_root(html_dir)?;
    }

    let dump = choose_dump(&find_sql_dumps(&staging)?)?;
    Ok(PreparedBundle { dump })
}

fn extract_archive_to_dir(archive: &Path, target_dir: &Path) -> Result<()> {
    if !archive.exists() {
        bail!("archive no existe: {}", archive.display());
    }
    let raw = archive.to_string_lossy().to_ascii_lowercase();
    let status = if raw.ends_with(".tar.gz") || raw.ends_with(".tgz") {
        Command::new("tar")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(target_dir)
            .status()
            .wrap_err("extrayendo tar.gz")?
    } else if raw.ends_with(".tar") {
        Command::new("tar")
            .arg("-xf")
            .arg(archive)
            .arg("-C")
            .arg(target_dir)
            .status()
            .wrap_err("extrayendo tar")?
    } else if raw.ends_with(".zip") {
        Command::new("unzip")
            .arg("-q")
            .arg(archive)
            .arg("-d")
            .arg(target_dir)
            .status()
            .wrap_err("extrayendo zip")?
    } else {
        bail!("archive no soportado: usa .tar.gz, .tgz, .tar o .zip");
    };
    if !status.success() {
        bail!("extracción falló para {}", archive.display());
    }
    Ok(())
}

fn find_sql_dumps(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dumps = Vec::new();
    collect_files(root, &mut |path| {
        let raw = path.to_string_lossy().to_ascii_lowercase();
        if raw.ends_with(".sql") || raw.ends_with(".sql.gz") {
            dumps.push(path.to_path_buf());
        }
        Ok(())
    })?;
    dumps.sort();
    Ok(dumps)
}

fn choose_dump(dumps: &[PathBuf]) -> Result<Option<PathBuf>> {
    match dumps.len() {
        0 => Ok(None),
        1 => Ok(Some(dumps[0].clone())),
        _ => {
            let preferred: Vec<_> = dumps
                .iter()
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| {
                            let name = name.to_ascii_lowercase();
                            name.contains("database")
                                || name.contains("db")
                                || name.contains("dump")
                                || name.contains("backup")
                        })
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            if preferred.len() == 1 {
                Ok(Some(preferred[0].clone()))
            } else {
                bail!(
                    "bundle contiene múltiples dumps SQL; pasa --dump explícito: {}",
                    dumps
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
}

fn find_wp_root(root: &Path) -> Result<Option<PathBuf>> {
    if looks_like_wp_root(root) {
        return Ok(Some(root.to_path_buf()));
    }

    let mut candidates = Vec::new();
    collect_dirs(root, &mut |path| {
        if looks_like_wp_root(path) {
            candidates.push(path.to_path_buf());
        }
        Ok(())
    })?;

    candidates.sort_by_key(|path| wp_root_score(path));
    Ok(candidates.into_iter().next())
}

fn wp_root_score(path: &Path) -> usize {
    let name_score = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| match name {
            "public_html" => 0,
            "wordpress" => 1,
            "www" => 2,
            _ => 3,
        })
        .unwrap_or(3);
    let content_score = if path.join("wp-content").is_dir() {
        0
    } else {
        1
    };
    name_score + content_score
}

fn copy_dir_contents(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).wrap_err_with(|| format!("creando {}", to.display()))?;
    for entry in fs::read_dir(from).wrap_err_with(|| format!("leyendo {}", from.display()))? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if target.exists() {
            bail!("destino ya existe al importar bundle: {}", target.display());
        }
        if source.is_dir() {
            copy_dir_recursive(&source, &target)?;
        } else {
            fs::copy(&source, &target).wrap_err_with(|| {
                format!("copiando {} -> {}", source.display(), target.display())
            })?;
        }
    }
    Ok(())
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).wrap_err_with(|| format!("creando {}", to.display()))?;
    for entry in fs::read_dir(from).wrap_err_with(|| format!("leyendo {}", from.display()))? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir_recursive(&source, &target)?;
        } else {
            fs::copy(&source, &target).wrap_err_with(|| {
                format!("copiando {} -> {}", source.display(), target.display())
            })?;
        }
    }
    Ok(())
}

fn collect_files<F>(root: &Path, visit: &mut F) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    for entry in fs::read_dir(root).wrap_err_with(|| format!("leyendo {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, visit)?;
        } else {
            visit(&path)?;
        }
    }
    Ok(())
}

fn collect_dirs<F>(root: &Path, visit: &mut F) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    for entry in fs::read_dir(root).wrap_err_with(|| format!("leyendo {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            visit(&path)?;
            collect_dirs(&path, visit)?;
        }
    }
    Ok(())
}

fn flatten_single_wp_root(html_dir: &Path) -> Result<()> {
    let entries = visible_entries(html_dir)?;
    if entries.len() != 1 || !entries[0].is_dir() || !looks_like_wp_root(&entries[0]) {
        return Ok(());
    }

    let root = entries[0].clone();
    for entry in fs::read_dir(&root).wrap_err_with(|| format!("leyendo {}", root.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = html_dir.join(entry.file_name());
        if to.exists() {
            bail!(
                "no se puede aplanar archive: destino ya existe {}",
                to.display()
            );
        }
        fs::rename(&from, &to)
            .wrap_err_with(|| format!("moviendo {} -> {}", from.display(), to.display()))?;
    }
    fs::remove_dir(&root).wrap_err_with(|| format!("eliminando {}", root.display()))?;
    Ok(())
}

fn visible_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).wrap_err_with(|| format!("leyendo {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy() == ".DS_Store" {
            continue;
        }
        entries.push(entry.path());
    }
    Ok(entries)
}

fn looks_like_wp_root(dir: &Path) -> bool {
    dir.join("wp-content").is_dir()
        || dir.join("wp-config.php").is_file()
        || dir.join("index.php").is_file()
}

fn backup_migrated_wp_config(html_dir: &Path) -> Result<()> {
    let config = html_dir.join("wp-config.php");
    if !config.exists() {
        return Ok(());
    }

    let mut backup = html_dir.join("wp-config.php.migrated.bak");
    if backup.exists() {
        backup = html_dir.join(format!(
            "wp-config.php.migrated.{}.bak",
            chrono::Utc::now().format("%Y%m%d%H%M%S")
        ));
    }

    fs::rename(&config, &backup)
        .wrap_err_with(|| format!("renombrando {} -> {}", config.display(), backup.display()))?;
    Ok(())
}

fn ensure_app_registered(
    store: &Store,
    client: &str,
    slug: &str,
    domain: &str,
    upstream: &str,
) -> Result<()> {
    if let Some(existing) = store
        .list_apps()?
        .into_iter()
        .find(|app| app.client_slug == client && app.slug == slug)
    {
        if existing.domain == domain && existing.upstream == upstream {
            return Ok(());
        }
        bail!(
            "app {}/{} ya existe con domain/upstream distinto: {} -> {}",
            client,
            slug,
            existing.domain,
            existing.upstream
        );
    }
    store.add_app(client, slug, domain, upstream)?;
    Ok(())
}

fn link_grant_to_app(
    store: &Store,
    server: &str,
    database: &str,
    username: &str,
    host: &str,
    client: &str,
    app: &str,
) -> Result<()> {
    let grant = store
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
                "grant no encontrado tras provisionar DB: server={} db={} user={} host={}",
                server,
                database,
                username,
                host
            )
        })?;
    store.reassign_database_grant(&grant.id, client, Some(app), Some("prod"))?;
    Ok(())
}

fn run_search_replace(
    site_dir: &Path,
    opts: &ProvisionOptions,
    old_domain: &str,
    new_domain: &str,
) -> Result<()> {
    let status = Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--network")
        .arg(&opts.network)
        .arg("--env-file")
        .arg(site_dir.join(".env"))
        .arg("-v")
        .arg(format!("{}:/var/www/html", site_dir.join("html").display()))
        .arg(&opts.cli_image)
        .arg("wp")
        .arg("search-replace")
        .arg(old_domain)
        .arg(new_domain)
        .arg("--all-tables")
        .arg("--skip-columns=guid")
        .arg("--allow-root")
        .status()
        .wrap_err("ejecutando wp search-replace")?;
    if !status.success() {
        bail!("wp search-replace falló: {} -> {}", old_domain, new_domain);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordpress_container_name_is_stable() {
        assert_eq!(
            wordpress_container_name("cliente-demo", "web_1"),
            "cliente_demo_web_1_wordpress"
        );
    }

    #[test]
    fn render_compose_uses_external_hosting_network() {
        let compose = render_compose(
            "wordpress:php8.5-apache",
            "cliente_web_wordpress",
            "hosting",
        );
        assert!(compose.contains("image: wordpress:php8.5-apache"));
        assert!(compose.contains("container_name: cliente_web_wordpress"));
        assert!(compose.contains("hosting:"));
        assert!(compose.contains("external: true"));
    }

    #[test]
    fn write_env_file_uses_given_wp_db_host() {
        let tmp = tempfile_dir("wp-env");
        fs::create_dir_all(&tmp).unwrap();
        let env_path = tmp.join(".env");

        write_env_file(
            &env_path,
            "client_web_prod",
            "client_web_user",
            "secret",
            "mariadb:3306",
        )
        .unwrap();

        let raw = fs::read_to_string(&env_path).unwrap();
        assert!(raw.contains("WORDPRESS_DB_HOST=mariadb:3306"));
        assert!(raw.contains("WORDPRESS_DB_NAME=client_web_prod"));
        assert_eq!(
            fs::metadata(&env_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn choose_dump_selects_single_dump() {
        let dumps = vec![PathBuf::from("backup.sql.gz")];
        assert_eq!(
            choose_dump(&dumps).unwrap().as_deref(),
            Some(dumps[0].as_path())
        );
    }

    #[test]
    fn choose_dump_prefers_named_backup_when_multiple() {
        let dumps = vec![
            PathBuf::from("random.sql"),
            PathBuf::from("database-backup.sql.gz"),
        ];
        assert_eq!(
            choose_dump(&dumps).unwrap().as_deref(),
            Some(dumps[1].as_path())
        );
    }

    #[test]
    fn find_wp_root_prefers_public_html() {
        let tmp = tempfile_dir("wp-root");
        fs::create_dir_all(tmp.join("nested").join("wp-content")).unwrap();
        fs::create_dir_all(tmp.join("public_html").join("wp-content")).unwrap();

        let root = find_wp_root(&tmp).unwrap().unwrap();

        assert_eq!(
            root.file_name().and_then(|name| name.to_str()),
            Some("public_html")
        );
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn backup_migrated_wp_config_renames_existing_config() {
        let tmp = tempfile_dir("wp-config-backup");
        let html = tmp.join("html");
        fs::create_dir_all(&html).unwrap();
        fs::write(html.join("wp-config.php"), "<?php old config").unwrap();

        backup_migrated_wp_config(&html).unwrap();

        assert!(!html.join("wp-config.php").exists());
        assert!(html.join("wp-config.php.migrated.bak").is_file());
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn ensure_app_registered_allows_matching_existing_app() {
        let db = std::env::temp_dir().join(format!(
            "hostingctl-wp-app-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(&db).unwrap();
        store.add_client("client", "Client", None).unwrap();
        store
            .add_app("client", "web", "example.com", "client_web_wordpress:80")
            .unwrap();

        ensure_app_registered(
            &store,
            "client",
            "web",
            "example.com",
            "client_web_wordpress:80",
        )
        .unwrap();

        assert_eq!(store.list_apps().unwrap().len(), 1);
        let _ = fs::remove_file(db);
    }

    #[test]
    fn flatten_single_wp_root_moves_nested_site_to_html_root() {
        let tmp = tempfile_dir("wp-flatten");
        let html = tmp.join("html");
        let nested = html.join("public_html");
        fs::create_dir_all(nested.join("wp-content")).unwrap();
        fs::write(nested.join("index.php"), "<?php").unwrap();

        flatten_single_wp_root(&html).unwrap();

        assert!(html.join("wp-content").is_dir());
        assert!(html.join("index.php").is_file());
        assert!(!nested.exists());
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn flatten_single_wp_root_ignores_multi_entry_html_dir() {
        let tmp = tempfile_dir("wp-no-flatten");
        let html = tmp.join("html");
        fs::create_dir_all(html.join("public_html").join("wp-content")).unwrap();
        fs::create_dir_all(html.join("other")).unwrap();

        flatten_single_wp_root(&html).unwrap();

        assert!(html.join("public_html").join("wp-content").is_dir());
        let _ = fs::remove_dir_all(tmp);
    }

    fn tempfile_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hostingctl-{prefix}-{}", uuid::Uuid::new_v4()))
    }
}
