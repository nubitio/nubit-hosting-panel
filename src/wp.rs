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
    store::{App, DbServer, ProvisionedDb, Store, convention_names, validate_slug},
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

#[derive(Debug)]
pub struct HardenSummary {
    pub site_dir: PathBuf,
    pub html_dir: PathBuf,
    pub wp_config_hardened: bool,
    pub permissions_normalized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuditCheck {
    pub status: CheckStatus,
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ScanFinding {
    pub status: CheckStatus,
    pub path: PathBuf,
    pub detail: String,
}

#[derive(Debug)]
pub struct CanonicalizeDomainOptions {
    pub client: String,
    pub slug: String,
    pub domain: String,
    pub sites_dir: PathBuf,
    pub network: String,
    pub cli_image: String,
    pub dry_run: bool,
}

#[derive(Debug)]
pub struct CanonicalizeDomainSummary {
    pub site_dir: PathBuf,
    pub canonical_url: String,
    pub steps: Vec<String>,
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
    let canonical_domain = canonical_domain(&opts.domain);
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

    normalize_wp_permissions(&html_dir)?;

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

    docker::ensure_network(&opts.network)?;
    docker::compose_pull(&site_dir)?;
    docker::compose_up(&site_dir)?;
    harden_html_dir(&html_dir)?;

    if let Some(dump) = dump_path {
        backup::restore(cfg, &server, &provisioned.database, dump)
            .wrap_err_with(|| format!("importando dump {}", dump.display()))?;
    }

    let app = ensure_app_registered(
        store,
        &opts.client,
        &opts.slug,
        &canonical_domain,
        &upstream,
    )?;
    ensure_www_redirect_alias(store, &app, &canonical_domain)?;
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
        run_search_replace(&site_dir, &opts, old_domain, &canonical_domain)?;
    }

    if opts.mode == ProvisionMode::Existing {
        canonicalize_domain_at_site(
            &site_dir,
            &opts.network,
            &opts.cli_image,
            &canonical_domain,
            false,
        )
        .wrap_err_with(|| {
            format!("canonicalizando dominio WordPress a https://{canonical_domain}")
        })?;
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

pub fn harden_site(sites_dir: &Path, client: &str, slug: &str) -> Result<HardenSummary> {
    validate_slug(client)?;
    validate_slug(slug)?;
    let site_dir = sites_dir.join(client).join(slug);
    let html_dir = site_dir.join("html");
    if !html_dir.is_dir() {
        bail!("html dir no existe: {}", html_dir.display());
    }
    let wp_config_hardened = harden_wp_config(&html_dir)?;
    normalize_wp_permissions(&html_dir)?;
    Ok(HardenSummary {
        site_dir,
        html_dir,
        wp_config_hardened,
        permissions_normalized: true,
    })
}

pub fn audit_site(sites_dir: &Path, client: &str, slug: &str) -> Result<Vec<AuditCheck>> {
    validate_slug(client)?;
    validate_slug(slug)?;
    let html_dir = sites_dir.join(client).join(slug).join("html");
    if !html_dir.is_dir() {
        bail!("html dir no existe: {}", html_dir.display());
    }

    let mut checks = Vec::new();
    audit_wp_config(&html_dir, &mut checks)?;
    audit_permissions(&html_dir, &mut checks)?;
    audit_risky_files(&html_dir, &mut checks)?;
    audit_uploads_php(&html_dir, &mut checks)?;
    Ok(checks)
}

pub fn scan_site(sites_dir: &Path, client: &str, slug: &str) -> Result<Vec<ScanFinding>> {
    validate_slug(client)?;
    validate_slug(slug)?;
    let html_dir = sites_dir.join(client).join(slug).join("html");
    if !html_dir.is_dir() {
        bail!("html dir no existe: {}", html_dir.display());
    }

    let mut findings = Vec::new();
    collect_files(&html_dir, &mut |path| {
        scan_file(&html_dir, path, &mut findings)?;
        Ok(())
    })?;
    Ok(findings)
}

pub fn canonicalize_domain(opts: CanonicalizeDomainOptions) -> Result<CanonicalizeDomainSummary> {
    validate_slug(&opts.client)?;
    validate_slug(&opts.slug)?;
    validate_slug(&opts.network)?;
    let domain = canonical_domain(&opts.domain);
    let site_dir = opts.sites_dir.join(&opts.client).join(&opts.slug);

    canonicalize_domain_at_site(
        &site_dir,
        &opts.network,
        &opts.cli_image,
        &domain,
        opts.dry_run,
    )
}

fn canonicalize_domain_at_site(
    site_dir: &Path,
    network: &str,
    cli_image: &str,
    domain: &str,
    dry_run: bool,
) -> Result<CanonicalizeDomainSummary> {
    let canonical_url = format!("https://{domain}");
    let html_dir = site_dir.join("html");
    let env_file = site_dir.join(".env");
    if !html_dir.is_dir() {
        bail!("html dir no existe: {}", html_dir.display());
    }
    if !env_file.is_file() {
        bail!("env file no existe: {}", env_file.display());
    }

    let mut steps = Vec::new();
    if dry_run {
        steps.push("dry-run: no se actualizan home/siteurl ni se purga caché".to_string());
    } else {
        run_wp_cli(
            site_dir,
            network,
            cli_image,
            &["option", "update", "home", &canonical_url],
        )?;
        steps.push(format!("home => {canonical_url}"));
        run_wp_cli(
            site_dir,
            network,
            cli_image,
            &["option", "update", "siteurl", &canonical_url],
        )?;
        steps.push(format!("siteurl => {canonical_url}"));
    }

    let replacements = [
        (format!("https://www.{domain}"), canonical_url.clone()),
        (format!("http://www.{domain}"), canonical_url.clone()),
        (format!("http://{domain}"), canonical_url.clone()),
    ];
    for (old, new) in replacements {
        let mut args = vec![
            "search-replace".to_string(),
            old.clone(),
            new.clone(),
            "--all-tables".to_string(),
            "--skip-columns=guid".to_string(),
        ];
        if dry_run {
            args.push("--dry-run".to_string());
        }
        run_wp_cli_owned(site_dir, network, cli_image, &args)?;
        steps.push(format!(
            "{}{} -> {}",
            if dry_run { "dry-run: " } else { "" },
            old,
            new
        ));
    }

    if !dry_run {
        run_wp_cli(site_dir, network, cli_image, &["cache", "flush"])?;
        steps.push("cache flush".to_string());
    }

    Ok(CanonicalizeDomainSummary {
        site_dir: site_dir.to_path_buf(),
        canonical_url,
        steps,
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

fn normalize_wp_permissions(html_dir: &Path) -> Result<()> {
    if !html_dir.exists() {
        return Ok(());
    }

    normalize_path_permissions(html_dir)?;
    collect_paths(html_dir, &mut |path| normalize_path_permissions(path))?;
    let wp_config = html_dir.join("wp-config.php");
    if wp_config.is_file() {
        let mut permissions = fs::metadata(&wp_config)?.permissions();
        permissions.set_mode(0o640);
        fs::set_permissions(&wp_config, permissions)
            .wrap_err_with(|| format!("ajustando permisos de {}", wp_config.display()))?;
    }
    Ok(())
}

fn normalize_path_permissions(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("leyendo permisos de {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    let mode = if metadata.is_dir() { 0o755 } else { 0o644 };
    let mut permissions = metadata.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
        .wrap_err_with(|| format!("ajustando permisos de {}", path.display()))?;
    Ok(())
}

fn collect_paths<F>(root: &Path, visit: &mut F) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    for entry in fs::read_dir(root).wrap_err_with(|| format!("leyendo {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        visit(&path)?;
        if path.is_dir() {
            collect_paths(&path, visit)?;
        }
    }
    Ok(())
}

fn harden_html_dir(html_dir: &Path) -> Result<()> {
    harden_wp_config(html_dir)?;
    normalize_wp_permissions(html_dir)?;
    Ok(())
}

fn harden_wp_config(html_dir: &Path) -> Result<bool> {
    let path = html_dir.join("wp-config.php");
    if !path.is_file() {
        return Ok(false);
    }

    let raw = fs::read_to_string(&path).wrap_err_with(|| format!("leyendo {}", path.display()))?;
    let mut additions = Vec::new();
    if !raw.contains("DISALLOW_FILE_EDIT") {
        additions.push("define('DISALLOW_FILE_EDIT', true);");
    }
    if !raw.contains("WP_AUTO_UPDATE_CORE") {
        additions.push("define('WP_AUTO_UPDATE_CORE', 'minor');");
    }
    if additions.is_empty() {
        return Ok(false);
    }

    let block = format!(
        "\n// Managed by hostingctl WordPress hardening.\n{}\n",
        additions.join("\n")
    );
    let next = if let Some(idx) = raw.find("/* That's all, stop editing") {
        let mut next = String::with_capacity(raw.len() + block.len());
        next.push_str(&raw[..idx]);
        next.push_str(&block);
        next.push_str(&raw[idx..]);
        next
    } else {
        let mut next = raw.trim_end().to_string();
        next.push_str(&block);
        next.push('\n');
        next
    };
    fs::write(&path, next).wrap_err_with(|| format!("escribiendo {}", path.display()))?;
    Ok(true)
}

fn audit_wp_config(html_dir: &Path, checks: &mut Vec<AuditCheck>) -> Result<()> {
    let path = html_dir.join("wp-config.php");
    if !path.is_file() {
        checks.push(AuditCheck {
            status: CheckStatus::Warn,
            name: "wp-config.php".to_string(),
            detail: "no encontrado; puede estar generado por la imagen o faltar en migración"
                .to_string(),
        });
        return Ok(());
    }

    let raw = fs::read_to_string(&path).wrap_err_with(|| format!("leyendo {}", path.display()))?;
    checks.push(AuditCheck {
        status: if raw.contains("DISALLOW_FILE_EDIT") {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        name: "DISALLOW_FILE_EDIT".to_string(),
        detail: if raw.contains("DISALLOW_FILE_EDIT") {
            "editor de archivos desde wp-admin deshabilitado".to_string()
        } else {
            "falta define('DISALLOW_FILE_EDIT', true)".to_string()
        },
    });
    checks.push(AuditCheck {
        status: if raw.contains("WP_AUTO_UPDATE_CORE") {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        name: "WP_AUTO_UPDATE_CORE".to_string(),
        detail: if raw.contains("WP_AUTO_UPDATE_CORE") {
            "auto-update core configurado".to_string()
        } else {
            "falta política explícita de auto-update core".to_string()
        },
    });
    Ok(())
}

fn audit_permissions(html_dir: &Path, checks: &mut Vec<AuditCheck>) -> Result<()> {
    let mut bad_dirs = 0;
    let mut bad_files = 0;
    collect_paths(html_dir, &mut |path| {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        let mode = metadata.permissions().mode() & 0o777;
        if metadata.is_dir() {
            if mode & 0o022 != 0 || mode & 0o111 != 0o111 {
                bad_dirs += 1;
            }
        } else if mode & 0o022 != 0 || mode & 0o444 != 0o444 {
            bad_files += 1;
        }
        Ok(())
    })?;
    let wp_config = html_dir.join("wp-config.php");
    let wp_config_mode = fs::metadata(&wp_config)
        .ok()
        .map(|m| m.permissions().mode() & 0o777);
    checks.push(AuditCheck {
        status: if bad_dirs == 0 && bad_files == 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        name: "permisos árbol html".to_string(),
        detail: format!("dirs inseguros: {bad_dirs}, files inseguros: {bad_files}"),
    });
    if let Some(mode) = wp_config_mode {
        checks.push(AuditCheck {
            status: if mode <= 0o640 {
                CheckStatus::Pass
            } else {
                CheckStatus::Warn
            },
            name: "permisos wp-config.php".to_string(),
            detail: format!("modo actual: {mode:o}; recomendado <= 640"),
        });
    }
    Ok(())
}

fn audit_risky_files(html_dir: &Path, checks: &mut Vec<AuditCheck>) -> Result<()> {
    let risky = [".env", "readme.html", "license.txt"];
    let found: Vec<_> = risky
        .iter()
        .filter(|name| html_dir.join(name).exists())
        .map(|name| name.to_string())
        .collect();
    checks.push(AuditCheck {
        status: if found.is_empty() {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        name: "archivos públicos riesgosos".to_string(),
        detail: if found.is_empty() {
            "no detectados".to_string()
        } else {
            found.join(", ")
        },
    });
    Ok(())
}

fn audit_uploads_php(html_dir: &Path, checks: &mut Vec<AuditCheck>) -> Result<()> {
    let uploads = html_dir.join("wp-content").join("uploads");
    if !uploads.is_dir() {
        checks.push(AuditCheck {
            status: CheckStatus::Pass,
            name: "PHP en uploads".to_string(),
            detail: "uploads no existe o no tiene PHP".to_string(),
        });
        return Ok(());
    }
    let mut count = 0;
    collect_files(&uploads, &mut |path| {
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("php"))
            .unwrap_or(false)
        {
            count += 1;
        }
        Ok(())
    })?;
    checks.push(AuditCheck {
        status: if count == 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        name: "PHP en uploads".to_string(),
        detail: format!("archivos PHP encontrados: {count}"),
    });
    Ok(())
}

fn scan_file(html_dir: &Path, path: &Path, findings: &mut Vec<ScanFinding>) -> Result<()> {
    let rel = path.strip_prefix(html_dir).unwrap_or(path).to_path_buf();
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let in_uploads = rel.starts_with(Path::new("wp-content/uploads"));

    let should_scan_text = matches!(
        ext.as_str(),
        "php" | "phtml" | "js" | "txt" | "ico" | "jpg" | "jpeg" | "png" | "gif"
    ) && fs::metadata(path)
        .map(|m| m.len() <= 2_000_000)
        .unwrap_or(false);
    if !should_scan_text {
        return Ok(());
    }
    let bytes = fs::read(path).wrap_err_with(|| format!("leyendo {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();

    if in_uploads && ext == "php" && !is_benign_index_php(&rel, &text) {
        findings.push(ScanFinding {
            status: CheckStatus::Fail,
            path: rel.clone(),
            detail: "archivo PHP ejecutable dentro de uploads".to_string(),
        });
    }

    if is_media_extension(&ext) && text.contains("<?php") {
        findings.push(ScanFinding {
            status: CheckStatus::Fail,
            path: rel.clone(),
            detail: "archivo media contiene código PHP embebido".to_string(),
        });
    }

    let critical_patterns = [
        ("eval(base64_decode(", "eval(base64_decode())"),
        ("eval(gzinflate(", "eval(gzinflate())"),
        ("assert($_", "assert() con input de usuario"),
        ("eval($_", "eval() con input de usuario"),
        ("system($_", "system() con input de usuario"),
        ("shell_exec($_", "shell_exec() con input de usuario"),
        ("passthru($_", "passthru() con input de usuario"),
        ("preg_replace('/.*/e", "preg_replace /e"),
        ("filesman", "firma FilesMan"),
        ("c99shell", "firma c99shell"),
        ("r57shell", "firma r57shell"),
    ];
    let critical: Vec<_> = critical_patterns
        .iter()
        .filter(|(needle, _)| text.contains(*needle))
        .map(|(_, label)| *label)
        .collect();
    if !critical.is_empty() {
        findings.push(ScanFinding {
            status: CheckStatus::Fail,
            path: rel.clone(),
            detail: format!("indicadores críticos: {}", critical.join(", ")),
        });
    }

    if ext != "php" && ext != "phtml" {
        return Ok(());
    }

    let generic_patterns = [
        "eval(",
        "base64_decode(",
        "gzinflate(",
        "shell_exec(",
        "passthru(",
        "assert(",
    ];
    let matched: Vec<_> = generic_patterns
        .iter()
        .filter(|p| text.contains(**p))
        .copied()
        .collect();
    if !matched.is_empty() && !is_noise_prone_vendor_path(&rel) {
        findings.push(ScanFinding {
            status: CheckStatus::Warn,
            path: rel,
            detail: format!("patrones sospechosos: {}", matched.join(", ")),
        });
    }
    Ok(())
}

fn is_media_extension(ext: &str) -> bool {
    matches!(ext, "ico" | "jpg" | "jpeg" | "png" | "gif")
}

fn is_benign_index_php(rel: &Path, text: &str) -> bool {
    rel.file_name()
        .and_then(|s| s.to_str())
        .map(|name| name.eq_ignore_ascii_case("index.php"))
        .unwrap_or(false)
        && !text.contains("$_")
        && !text.contains("eval(")
        && !text.contains("base64_decode(")
        && !text.contains("gzinflate(")
        && !text.contains("shell_exec(")
        && !text.contains("passthru(")
        && text.trim().len() <= 256
}

fn is_noise_prone_vendor_path(rel: &Path) -> bool {
    rel.starts_with(Path::new("wp-includes"))
        || rel.starts_with(Path::new("wp-admin"))
        || path_has_component(rel, "vendor")
        || path_has_component(rel, "vendor_prefixed")
        || path_has_component(rel, "node_modules")
        || path_has_component(rel, "dist")
        || path_has_component(rel, "build")
}

fn path_has_component(path: &Path, needle: &str) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|part| part.eq_ignore_ascii_case(needle))
            .unwrap_or(false)
    })
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

fn canonical_domain(domain: &str) -> String {
    let normalized = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    normalized
        .strip_prefix("www.")
        .unwrap_or(&normalized)
        .to_string()
}

fn www_domain_for(canonical_domain: &str) -> Option<String> {
    if canonical_domain.is_empty()
        || canonical_domain.starts_with("www.")
        || !canonical_domain.contains('.')
    {
        None
    } else {
        Some(format!("www.{canonical_domain}"))
    }
}

fn ensure_www_redirect_alias(store: &Store, app: &App, canonical_domain: &str) -> Result<()> {
    let Some(www_domain) = www_domain_for(canonical_domain) else {
        return Ok(());
    };

    if store
        .list_domain_aliases()?
        .into_iter()
        .any(|alias| alias.app_id == app.id && alias.domain.eq_ignore_ascii_case(&www_domain))
    {
        return Ok(());
    }

    store
        .add_domain_alias(&app.id, &www_domain)
        .wrap_err_with(|| format!("registrando alias canonical www: {www_domain}"))?;
    Ok(())
}

fn ensure_app_registered(
    store: &Store,
    client: &str,
    slug: &str,
    domain: &str,
    upstream: &str,
) -> Result<App> {
    if let Some(existing) = store
        .list_apps()?
        .into_iter()
        .find(|app| app.client_slug == client && app.slug == slug)
    {
        if existing.domain == domain && existing.upstream == upstream {
            return Ok(existing);
        }
        bail!(
            "app {}/{} ya existe con domain/upstream distinto: {} -> {}",
            client,
            slug,
            existing.domain,
            existing.upstream
        );
    }
    store.add_app(client, slug, domain, upstream)
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
    run_wp_cli(
        site_dir,
        &opts.network,
        &opts.cli_image,
        &[
            "search-replace",
            old_domain,
            new_domain,
            "--all-tables",
            "--skip-columns=guid",
        ],
    )
    .wrap_err_with(|| format!("wp search-replace falló: {} -> {}", old_domain, new_domain))
}

fn run_wp_cli(site_dir: &Path, network: &str, cli_image: &str, args: &[&str]) -> Result<()> {
    let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    run_wp_cli_owned(site_dir, network, cli_image, &owned)
}

fn run_wp_cli_owned(
    site_dir: &Path,
    network: &str,
    cli_image: &str,
    args: &[String],
) -> Result<()> {
    let status = Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--user")
        .arg("0:0")
        .arg("--network")
        .arg(network)
        .arg("--env-file")
        .arg(site_dir.join(".env"))
        .arg("-v")
        .arg(format!("{}:/var/www/html", site_dir.join("html").display()))
        .arg(cli_image)
        .arg("wp")
        .args(args)
        .arg("--path=/var/www/html")
        .arg("--allow-root")
        .status()
        .wrap_err("ejecutando wp-cli en Docker")?;
    if !status.success() {
        bail!("wp-cli falló: wp {}", args.join(" "));
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
    fn normalize_wp_permissions_makes_htaccess_readable_by_apache() {
        let tmp = tempfile_dir("wp-permissions");
        let html = tmp.join("html");
        let uploads = html.join("wp-content").join("uploads");
        fs::create_dir_all(&uploads).unwrap();
        fs::write(html.join(".htaccess"), "# wordpress").unwrap();
        fs::write(uploads.join("image.jpg"), "img").unwrap();

        fs::set_permissions(&html, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(html.join(".htaccess"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&uploads, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(uploads.join("image.jpg"), fs::Permissions::from_mode(0o600)).unwrap();

        normalize_wp_permissions(&html).unwrap();

        assert_eq!(
            fs::metadata(&html).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(html.join(".htaccess"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        assert_eq!(
            fs::metadata(&uploads).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(uploads.join("image.jpg"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn harden_wp_config_inserts_security_defines() {
        let tmp = tempfile_dir("wp-config-harden");
        let html = tmp.join("html");
        fs::create_dir_all(&html).unwrap();
        fs::write(
            html.join("wp-config.php"),
            "<?php\ndefine('DB_NAME', 'wp');\n/* That's all, stop editing! Happy publishing. */\n",
        )
        .unwrap();

        assert!(harden_wp_config(&html).unwrap());
        assert!(!harden_wp_config(&html).unwrap());

        let raw = fs::read_to_string(html.join("wp-config.php")).unwrap();
        assert!(raw.contains("define('DISALLOW_FILE_EDIT', true);"));
        assert!(raw.contains("define('WP_AUTO_UPDATE_CORE', 'minor');"));
        assert_eq!(raw.matches("DISALLOW_FILE_EDIT").count(), 1);
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn audit_site_reports_uploads_php_as_fail() {
        let tmp = tempfile_dir("wp-audit");
        let html = tmp.join("client").join("web").join("html");
        fs::create_dir_all(html.join("wp-content").join("uploads")).unwrap();
        fs::write(
            html.join("wp-config.php"),
            "<?php define('DISALLOW_FILE_EDIT', true); define('WP_AUTO_UPDATE_CORE', 'minor');",
        )
        .unwrap();
        fs::write(
            html.join("wp-content").join("uploads").join("shell.php"),
            "<?php",
        )
        .unwrap();

        let checks = audit_site(&tmp, "client", "web").unwrap();

        assert!(
            checks
                .iter()
                .any(|check| check.name == "PHP en uploads" && check.status == CheckStatus::Fail)
        );
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn scan_site_detects_suspicious_patterns() {
        let tmp = tempfile_dir("wp-scan");
        let html = tmp.join("client").join("web").join("html");
        fs::create_dir_all(html.join("wp-content").join("uploads")).unwrap();
        fs::write(
            html.join("wp-content").join("uploads").join("shell.php"),
            "<?php eval(base64_decode($_POST['x']));",
        )
        .unwrap();

        let findings = scan_site(&tmp, "client", "web").unwrap();

        assert!(findings.iter().any(
            |finding| finding.status == CheckStatus::Fail && finding.detail.contains("uploads")
        ));
        assert!(
            findings
                .iter()
                .any(|finding| finding.status == CheckStatus::Warn
                    && finding.detail.contains("base64_decode"))
        );
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn scan_site_ignores_benign_uploads_index_php() {
        let tmp = tempfile_dir("wp-scan-benign-index");
        let html = tmp.join("client").join("web").join("html");
        fs::create_dir_all(html.join("wp-content").join("uploads").join("smush")).unwrap();
        fs::write(
            html.join("wp-content")
                .join("uploads")
                .join("smush")
                .join("index.php"),
            "<?php\n// Silence is golden.\n",
        )
        .unwrap();

        let findings = scan_site(&tmp, "client", "web").unwrap();

        assert!(findings.is_empty());
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn scan_site_suppresses_generic_wordpress_core_noise() {
        let tmp = tempfile_dir("wp-scan-core-noise");
        let html = tmp.join("client").join("web").join("html");
        fs::create_dir_all(html.join("wp-includes")).unwrap();
        fs::write(
            html.join("wp-includes").join("class-json.php"),
            "<?php function ok() { eval('$legacy'); }",
        )
        .unwrap();

        let findings = scan_site(&tmp, "client", "web").unwrap();

        assert!(findings.is_empty());
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn scan_site_flags_php_embedded_in_media() {
        let tmp = tempfile_dir("wp-scan-media-php");
        let html = tmp.join("client").join("web").join("html");
        fs::create_dir_all(html.join("wp-content").join("uploads")).unwrap();
        fs::write(
            html.join("wp-content").join("uploads").join("logo.png"),
            "PNG bytes <?php eval($_POST['x']);",
        )
        .unwrap();

        let findings = scan_site(&tmp, "client", "web").unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.status == CheckStatus::Fail)
        );
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
    fn canonical_domain_strips_www_and_normalizes_case() {
        assert_eq!(canonical_domain("WWW.Dimexa.COM.PE."), "dimexa.com.pe");
        assert_eq!(canonical_domain("dimexa.com.pe"), "dimexa.com.pe");
    }

    #[test]
    fn ensure_www_redirect_alias_adds_www_alias_once() {
        let db = std::env::temp_dir().join(format!(
            "hostingctl-wp-www-alias-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(&db).unwrap();
        store.add_client("client", "Client", None).unwrap();
        let app = store
            .add_app("client", "web", "example.com", "client_web_wordpress:80")
            .unwrap();

        ensure_www_redirect_alias(&store, &app, "example.com").unwrap();
        ensure_www_redirect_alias(&store, &app, "example.com").unwrap();

        let aliases = store.list_domain_aliases().unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].domain, "www.example.com");
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
