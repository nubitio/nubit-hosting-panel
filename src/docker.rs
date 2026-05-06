use std::{
    io,
    process::{Command, Stdio},
};

use color_eyre::eyre::{Result, bail};

pub fn container_name_from_upstream(upstream: &str) -> Option<String> {
    if upstream.starts_with("http://") || upstream.starts_with("https://") {
        return None;
    }
    let host = upstream.split(':').next().unwrap_or("").trim();
    let is_ip =
        host == "localhost" || host.starts_with("127.") || host.parse::<std::net::IpAddr>().is_ok();
    if host.is_empty() || is_ip {
        None
    } else {
        Some(host.to_string())
    }
}

pub fn logs(container: &str, tail: usize, follow: bool, since: Option<&str>) -> Result<()> {
    let mut cmd = Command::new("docker");
    cmd.arg("logs").arg("--tail").arg(tail.to_string());
    if follow {
        cmd.arg("--follow");
    }
    if let Some(since) = since {
        cmd.arg("--since").arg(since);
    }
    let status = cmd.arg(container).status()?;
    if !status.success() {
        bail!("docker logs falló para {container}");
    }
    Ok(())
}

pub fn exec(container: &str, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("exec requiere comando");
    }
    let status = Command::new("docker")
        .arg("exec")
        .arg(container)
        .args(args)
        .status()?;
    if !status.success() {
        bail!("docker exec falló para {container}");
    }
    Ok(())
}

pub fn shell(container: &str, shell: &str) -> Result<()> {
    let mut cmd = Command::new("docker");
    cmd.args([
        "exec",
        "-it",
        "-e",
        "TERM=xterm-256color",
        "-e",
        "COLORTERM=truecolor",
        container,
    ]);
    if shell == "auto" {
        cmd.args([
            "sh",
            "-lc",
            "if command -v bash >/dev/null 2>&1; then exec bash -il; else exec sh -i; fi",
        ]);
    } else {
        cmd.arg(shell);
    }
    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        bail!("docker shell falló para {container}");
    }
    Ok(())
}

pub fn spawn_logs(container: &str, tail: usize, follow: bool) -> io::Result<std::process::Child> {
    let mut cmd = Command::new("docker");
    cmd.arg("logs").arg("--tail").arg(tail.to_string());
    if follow {
        cmd.arg("--follow");
    }
    cmd.arg(container)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

pub fn ensure_network(network: &str) -> Result<()> {
    let exists = Command::new("docker")
        .arg("network")
        .arg("inspect")
        .arg(network)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if exists.success() {
        return Ok(());
    }

    let created = Command::new("docker")
        .arg("network")
        .arg("create")
        .arg(network)
        .status()?;
    if !created.success() {
        bail!("docker network create falló para {network}");
    }
    Ok(())
}

pub fn compose_up(project_dir: &std::path::Path) -> Result<()> {
    let status = Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(project_dir.join("compose.yml"))
        .arg("up")
        .arg("-d")
        .status()?;
    if !status.success() {
        bail!("docker compose up falló en {}", project_dir.display());
    }
    Ok(())
}

pub fn compose_pull(project_dir: &std::path::Path) -> Result<()> {
    let status = Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(project_dir.join("compose.yml"))
        .arg("pull")
        .status()?;
    if !status.success() {
        bail!("docker compose pull falló en {}", project_dir.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_name_from_upstream_detects_local_container() {
        assert_eq!(
            container_name_from_upstream("wordpress_cliente:80").as_deref(),
            Some("wordpress_cliente")
        );
    }

    #[test]
    fn container_name_from_upstream_ignores_external_targets() {
        assert_eq!(container_name_from_upstream("https://example.com"), None);
        assert_eq!(container_name_from_upstream("10.0.0.5:8080"), None);
        assert_eq!(container_name_from_upstream("localhost:3000"), None);
    }
}
