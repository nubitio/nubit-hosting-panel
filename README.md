# Nubit Hosting Panel

`hostingctl` es el binario Rust autocontenido para operar clientes, sitios/apps, Caddy y bases de datos del hosting Nubit.

## Incluye

- CLI con `clap`.
- TUI básica con `ratatui`.
- Estado local en SQLite embebido.
- Slugs estrictos: `a-z`, `0-9`, `-`, `_`.
- Registro/listado de clientes.
- Registro/listado de sitios/apps/dominios por cliente.
- Export/import JSON de metadata del panel sin secretos.
- Caddy en archivo gestionado separado: `hostingctl.caddy`.
- `caddy bootstrap` para agregar `import hostingctl.caddy` al Caddyfile principal.
- `caddy apply` con backup y validación antes de reload.
- DB servers sin secretos en SQLite.
- Credenciales DB por env var o `config.toml`; env var tiene prioridad.
- MariaDB/MySQL: crear databases, crear usuarios, grants, provision, reset password, backup y restore local.
- CI con `cargo fmt --check`, `cargo clippy` y `cargo test`.
- GitHub Actions release Linux-only con artefactos `.tar.gz`, checksums e `install.sh`.

## Uso local

```sh
cargo run -- init
cargo run -- client add porteroseguro --name "Portero Seguro"
cargo run -- app add porteroseguro web \
  --domain porteroseguro.nubit.site \
  --upstream tomcat_porteroseguro:8080
cargo run -- caddy render
cargo run -- tui
```

## Caddy

Por defecto:

```txt
Caddyfile principal: /data/compose/1/conf/Caddyfile
Archivo gestionado: /data/compose/1/conf/hostingctl.caddy
```

Una sola vez:

```sh
hostingctl caddy bootstrap
```

Esto agrega al Caddyfile principal:

```txt
import hostingctl.caddy
```

Aplicar config gestionada y recargar:

```sh
hostingctl caddy apply --reload
```

Los comandos de validación/reload son configurables en `config.toml`:

```toml
caddy_validate_command = "docker exec caddy caddy validate --config /etc/caddy/Caddyfile"
caddy_reload_command = "docker exec caddy caddy reload --config /etc/caddy/Caddyfile"
```

## MariaDB

Registrar metadata no secreta del servidor:

```sh
hostingctl db server-add mariadb --kind mariadb --host 127.0.0.1 --port 3306
```

Definir credencial admin/root por env var:

```sh
export HOSTINGCTL_DB_MARIADB_URL='mysql://root:ROOT_PASSWORD@127.0.0.1:3306'
```

O en `config.toml`:

```toml
[db_servers.mariadb]
url = "mysql://root:ROOT_PASSWORD@127.0.0.1:3306"
```

Env var tiene prioridad sobre `config.toml`.

Provisionar DB completa para app:

```sh
hostingctl db provision mariadb porteroseguro web --generate
```

Por defecto crea:

```txt
database: porteroseguro_web_prod
user:     porteroseguro_web_user
host:     %
```

Con overrides:

```sh
hostingctl db provision mariadb porteroseguro web \
  --env staging \
  --database custom_db \
  --username custom_user \
  --password 'secret' \
  --host '%'
```

El password no se guarda. Se muestra una sola vez.

Reset/rotación de password:

```sh
hostingctl db reset-password mariadb porteroseguro_web_user --host '%' --generate
```

Comandos bajos disponibles:

```sh
hostingctl db create-database mariadb porteroseguro porteroseguro_web_prod
hostingctl db create-user mariadb porteroseguro_web_user --generate --host '%'
hostingctl db grant mariadb porteroseguro porteroseguro_web_prod porteroseguro_web_user --host '%'
```

Backup/restore local usando `docker exec <server>` y `mariadb-dump`/`mariadb` dentro del contenedor:

```sh
hostingctl db backup mariadb porteroseguro_web_prod --out ./backups
hostingctl db backup-list --out ./backups --server mariadb --database porteroseguro_web_prod
hostingctl db restore mariadb porteroseguro_web_prod ./backups/mariadb/porteroseguro_web_prod/20260504-153000.sql.gz --dry-run
hostingctl db restore mariadb porteroseguro_web_prod ./backups/mariadb/porteroseguro_web_prod/20260504-153000.sql.gz --yes
```

El backup genera `.sql.gz` en:

```txt
./backups/{server}/{database}/{timestamp}.sql.gz
```

## Export/import metadata

Exporta clientes, apps, DB servers metadata y grants. No exporta passwords.

```sh
hostingctl export --out hostingctl-export.json
hostingctl import hostingctl-export.json --dry-run
hostingctl import hostingctl-export.json --yes
```

Formato público versionado en JSON:

```json
{
  "version": 1,
  "exported_at": "...",
  "clients": [],
  "apps": [],
  "db_servers": [],
  "database_grants": []
}
```

## Release

Crear tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

Instalación Linux server:

```sh
curl -fsSL https://github.com/nubitio/nubit-hosting-panel/releases/latest/download/install.sh | sh
```

Por defecto instala en `/usr/local/bin`; puedes usar `--prefix`:

```sh
curl -fsSL https://github.com/nubitio/nubit-hosting-panel/releases/latest/download/install.sh | sh -s -- --prefix ~/.local
```

Si el repo/release es privado, usa un token con permiso de lectura:

```sh
export GITHUB_TOKEN='ghp_xxx'
curl -H "Authorization: Bearer ${GITHUB_TOKEN}" \
  -fsSL https://github.com/nubitio/nubit-hosting-panel/releases/latest/download/install.sh | sh
```

El installer también usa `GITHUB_TOKEN`/`HOSTINGCTL_GITHUB_TOKEN` para descargar binario y checksum.
