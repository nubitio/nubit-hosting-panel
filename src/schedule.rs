use std::{fs, path::Path, process::Command};

use color_eyre::eyre::{Context, Result, bail, eyre};

const SERVICE_NAME: &str = "hostingctl-backup";
const SERVICE_PATH: &str = "/etc/systemd/system/hostingctl-backup.service";
const TIMER_PATH: &str = "/etc/systemd/system/hostingctl-backup.timer";

pub struct TimerStatus {
    pub service_path: String,
    pub timer_path: String,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
}

pub fn install(schedule: &str, backup_dir: &Path, binary_path: &str) -> Result<()> {
    validate_schedule(schedule)?;

    let out_dir = backup_dir
        .canonicalize()
        .unwrap_or_else(|_| backup_dir.to_path_buf());
    let out_dir = out_dir.display();

    let service = format!(
        r#"[Unit]
Description=Nubit Hosting Panel — Automatic DB Backups
Documentation=https://github.com/nubitio/nubit-hosting-panel
After=docker.service network.target
Wants=docker.service

[Service]
Type=oneshot
ExecStart={binary_path} db backup-all --out {out_dir}
StandardOutput=journal
StandardError=journal
SyslogIdentifier={SERVICE_NAME}
"#
    );

    let timer = format!(
        r#"[Unit]
Description=Nubit Hosting Panel — Backup Timer ({schedule})
Requires={SERVICE_NAME}.service

[Timer]
OnCalendar={schedule}
Persistent=true
RandomizedDelaySec=300

[Install]
WantedBy=timers.target
"#
    );

    fs::write(SERVICE_PATH, service)
        .wrap_err_with(|| format!("escribiendo {SERVICE_PATH} (¿permisos de root?)"))?;
    fs::write(TIMER_PATH, timer).wrap_err_with(|| format!("escribiendo {TIMER_PATH}"))?;

    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", &format!("{SERVICE_NAME}.timer")])?;

    Ok(())
}

pub fn uninstall() -> Result<()> {
    let _ = run_systemctl(&["disable", "--now", &format!("{SERVICE_NAME}.timer")]);
    for path in [SERVICE_PATH, TIMER_PATH] {
        if Path::new(path).exists() {
            fs::remove_file(path).wrap_err_with(|| format!("eliminando {path}"))?;
        }
    }
    run_systemctl(&["daemon-reload"])?;
    Ok(())
}

pub fn status() -> Result<TimerStatus> {
    let enabled = Path::new(TIMER_PATH).exists()
        && Command::new("systemctl")
            .args(["is-enabled", &format!("{SERVICE_NAME}.timer")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    let last_run =
        systemctl_property(&format!("{SERVICE_NAME}.service"), "ExecMainExitTimestamp").ok();
    let next_run =
        systemctl_property(&format!("{SERVICE_NAME}.timer"), "NextElapseUSecRealtime").ok();

    Ok(TimerStatus {
        service_path: SERVICE_PATH.to_string(),
        timer_path: TIMER_PATH.to_string(),
        enabled,
        last_run: clean_timestamp(last_run),
        next_run: clean_timestamp(next_run),
    })
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .wrap_err("ejecutando systemctl")?;
    if !status.success() {
        bail!("systemctl {} falló", args.join(" "));
    }
    Ok(())
}

fn systemctl_property(unit: &str, property: &str) -> Result<String> {
    let out = Command::new("systemctl")
        .args(["show", unit, &format!("--property={property}")])
        .output()?;
    let raw = String::from_utf8_lossy(&out.stdout);
    raw.lines()
        .find(|l| l.starts_with(property))
        .map(|l| {
            l.split_once('=')
                .map(|(_, value)| value)
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .ok_or_else(|| eyre!("property not found"))
}

fn clean_timestamp(val: Option<String>) -> Option<String> {
    let v = val?;
    if v.is_empty() || v == "0" || v == "n/a" {
        None
    } else {
        Some(v)
    }
}

fn validate_schedule(schedule: &str) -> Result<()> {
    let common = ["daily", "weekly", "hourly", "monthly"];
    if common.contains(&schedule) {
        return Ok(());
    }
    // Accept systemd OnCalendar format e.g. "*-*-* 02:00:00"
    if schedule.contains(':') || schedule.contains('*') {
        return Ok(());
    }
    bail!(
        "schedule inválido `{}`; usa: daily, weekly, hourly, monthly, o formato OnCalendar",
        schedule
    );
}
