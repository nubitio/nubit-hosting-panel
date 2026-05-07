use std::{fs, process::Command};

use color_eyre::eyre::{Context, Result, eyre};

use crate::{
    config::Config,
    store::{App, DomainAlias},
};

pub fn render_block(apps: &[App], aliases: &[DomainAlias]) -> String {
    let mut out = String::new();
    out.push_str("# This file is managed by hostingctl. Do not edit manually.\n\n");
    for app in apps {
        let (redirect_aliases, proxy_aliases): (Vec<&DomainAlias>, Vec<&DomainAlias>) = aliases
            .iter()
            .filter(|a| a.app_id == app.id)
            .partition(|a| is_www_alias_for(&a.domain, &app.domain));

        for alias in redirect_aliases {
            out.push_str(&format!(
                "# client: {client} app: {slug} canonical redirect\n{alias} {{\n  redir https://{canonical}{{uri}} permanent\n}}\n\n",
                client = app.client_slug,
                slug = app.slug,
                alias = alias.domain,
                canonical = app.domain,
            ));
        }

        let extra: Vec<&str> = proxy_aliases.iter().map(|a| a.domain.as_str()).collect();
        let domains = if extra.is_empty() {
            app.domain.clone()
        } else {
            format!("{} {}", app.domain, extra.join(" "))
        };
        let hardening = render_wordpress_hardening(&app.upstream);
        let proxy = render_proxy(&app.upstream);
        out.push_str(&format!(
            "# client: {client} app: {slug}\n{domains} {{\n  encode zstd gzip\n{hardening}{proxy}\n}}\n\n",
            client = app.client_slug,
            slug = app.slug,
            domains = domains,
            hardening = hardening,
        ));
    }
    out
}

fn is_www_alias_for(alias: &str, canonical: &str) -> bool {
    alias
        .strip_prefix("www.")
        .map(|without_www| without_www.eq_ignore_ascii_case(canonical))
        .unwrap_or(false)
}

fn render_proxy(upstream: &str) -> String {
    if upstream.starts_with("https://") {
        format!("  reverse_proxy {upstream} {{\n    header_up Host {{upstream_hostport}}\n  }}")
    } else {
        format!("  reverse_proxy {upstream}")
    }
}

fn render_wordpress_hardening(upstream: &str) -> String {
    if !is_wordpress_upstream(upstream) {
        return String::new();
    }

    "  header {\n    X-Content-Type-Options nosniff\n    X-Frame-Options SAMEORIGIN\n    Referrer-Policy strict-origin-when-cross-origin\n  }\n  @wp_sensitive_path {\n    path /.env /.git* /wp-config.php /readme.html /license.txt /xmlrpc.php\n  }\n  respond @wp_sensitive_path 403\n  @wp_sensitive_ext {\n    path_regexp (?i).*[.](sql|sql[.]gz|tar|tar[.]gz|tgz|zip|bak)$\n  }\n  respond @wp_sensitive_ext 403\n  @wp_uploads_php {\n    path /wp-content/uploads/*.php /wp-content/uploads/*/*.php /wp-content/uploads/*/*/*.php /wp-content/uploads/*/*/*/*.php\n  }\n  respond @wp_uploads_php 403\n"
        .to_string()
}

fn is_wordpress_upstream(upstream: &str) -> bool {
    upstream.contains("_wordpress") || upstream.contains("wordpress")
}

pub fn bootstrap(cfg: &Config) -> Result<bool> {
    let path = &cfg.caddyfile_path;
    let import_line = format!("import {}", import_target(cfg));
    let existing = fs::read_to_string(path).unwrap_or_default();

    if existing.lines().any(|line| line.trim() == import_line) {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        backup(path)?;
    }

    let mut next = existing.trim_end().to_string();
    if !next.is_empty() {
        next.push_str("\n\n");
    }
    next.push_str(&import_line);
    next.push('\n');
    fs::write(path, next).wrap_err_with(|| format!("escribiendo {}", path.display()))?;
    Ok(true)
}

pub fn apply(cfg: &Config, apps: &[App], aliases: &[DomainAlias], reload: bool) -> Result<()> {
    let path = &cfg.caddy_managed_path;
    let previous = fs::read_to_string(path).ok();
    let next = render_block(apps, aliases);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        backup(path)?;
    }

    fs::write(path, next).wrap_err_with(|| format!("escribiendo {}", path.display()))?;

    if let Err(err) = validate(cfg) {
        match previous {
            Some(previous) => fs::write(path, previous)?,
            None => {
                let _ = fs::remove_file(path);
            }
        }
        return Err(err.wrap_err("Caddy validate falló; se restauró la config anterior"));
    }

    if reload {
        run_command(
            cfg.caddy_reload_command.as_deref(),
            "caddy_reload_command no está configurado",
            "falló reload de Caddy",
        )?;
    }

    Ok(())
}

fn validate(cfg: &Config) -> Result<()> {
    run_command(
        cfg.caddy_validate_command.as_deref(),
        "caddy_validate_command no está configurado",
        "falló validate de Caddy",
    )
}

fn run_command(command: Option<&str>, missing: &str, failed: &str) -> Result<()> {
    let command = command.ok_or_else(|| eyre!(missing.to_string()))?;
    let status = Command::new("sh").arg("-lc").arg(command).status()?;
    if !status.success() {
        return Err(eyre!("{failed}: {command}"));
    }
    Ok(())
}

fn backup(path: &std::path::Path) -> Result<()> {
    let backup = path.with_extension(format!(
        "backup-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    ));
    fs::copy(path, &backup).wrap_err_with(|| format!("creando backup {}", backup.display()))?;
    Ok(())
}

fn import_target(cfg: &Config) -> String {
    cfg.caddy_managed_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("hostingctl.caddy")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{App, DomainAlias};
    use chrono::Utc;

    #[test]
    fn render_block_contains_app_proxy() {
        let rendered = render_block(
            &[App {
                id: "app-1".into(),
                client_slug: "porteroseguro".into(),
                slug: "web".into(),
                domain: "porteroseguro.nubit.site".into(),
                upstream: "tomcat_porteroseguro:8080".into(),
                notes: None,
                created_at: Utc::now(),
            }],
            &[],
        );

        assert!(rendered.contains("# This file is managed by hostingctl"));
        assert!(rendered.contains("porteroseguro.nubit.site"));
        assert!(rendered.contains("reverse_proxy tomcat_porteroseguro:8080"));
    }

    #[test]
    fn render_block_sets_host_header_for_https_upstreams() {
        let rendered = render_block(
            &[App {
                id: "app-1".into(),
                client_slug: "external".into(),
                slug: "api".into(),
                domain: "api.nubit.site".into(),
                upstream: "https://api.example.com".into(),
                notes: None,
                created_at: Utc::now(),
            }],
            &[],
        );

        assert!(rendered.contains("reverse_proxy https://api.example.com {"));
        assert!(rendered.contains("header_up Host {upstream_hostport}"));
    }

    #[test]
    fn render_block_keeps_http_upstreams_simple() {
        let rendered = render_block(
            &[App {
                id: "app-1".into(),
                client_slug: "external".into(),
                slug: "api".into(),
                domain: "api.nubit.site".into(),
                upstream: "http://10.0.0.50:8080".into(),
                notes: None,
                created_at: Utc::now(),
            }],
            &[],
        );

        assert!(rendered.contains("reverse_proxy http://10.0.0.50:8080"));
        assert!(!rendered.contains("header_up Host"));
    }

    #[test]
    fn render_block_redirects_www_alias_to_canonical_domain() {
        let app = App {
            id: "app-1".into(),
            client_slug: "dimexa".into(),
            slug: "web".into(),
            domain: "dimexa.com.pe".into(),
            upstream: "dimexa_wordpress:80".into(),
            notes: None,
            created_at: Utc::now(),
        };
        let rendered = render_block(
            std::slice::from_ref(&app),
            &[DomainAlias {
                id: "alias-1".into(),
                app_id: app.id.clone(),
                domain: "www.dimexa.com.pe".into(),
                created_at: Utc::now(),
            }],
        );

        assert!(
            rendered.contains("www.dimexa.com.pe {\n  redir https://dimexa.com.pe{uri} permanent")
        );
        assert!(rendered.contains("dimexa.com.pe {\n  encode zstd gzip"));
        assert!(rendered.contains("reverse_proxy dimexa_wordpress:80"));
    }

    #[test]
    fn render_block_adds_wordpress_security_rules() {
        let rendered = render_block(
            &[App {
                id: "app-1".into(),
                client_slug: "client".into(),
                slug: "web".into(),
                domain: "example.com".into(),
                upstream: "client_web_wordpress:80".into(),
                notes: None,
                created_at: Utc::now(),
            }],
            &[],
        );

        assert!(rendered.contains("X-Content-Type-Options nosniff"));
        assert!(
            rendered
                .contains("path /.env /.git* /wp-config.php /readme.html /license.txt /xmlrpc.php")
        );
        assert!(rendered.contains("path_regexp (?i).*[.](sql|sql[.]gz|tar|tar[.]gz|tgz|zip|bak)$"));
        assert!(rendered.contains("/wp-content/uploads/*.php"));
    }
}
