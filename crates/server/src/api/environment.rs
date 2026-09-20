//! The environment the server runs with: every setting with where it came
//! from, the process environment with secrets hidden, and the served
//! directory's `cereyan.toml` with its secrets hidden. Hidden values never
//! leave the server.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use crate::state::AppState;

/// Written in place of a secret in `cereyan_toml`.
const TOML_MASK: &str = "••••••••";
/// Written in place of a password inside a URL.
const URL_MASK: &str = "••••••";

/// Name fragments that mark an environment variable as secret.
const SECRET_PARTS: &[&str] = &[
    "KEY",
    "SECRET",
    "TOKEN",
    "PASS",
    "PWD",
    "CREDENTIAL",
    "PRIVATE",
    "AUTH",
    "COOKIE",
    "SESSION",
];

/// The variables cereyan reads. They are shown as they are, except
/// `CEREYAN_TOKEN`, which is always hidden.
pub const KNOWN_VARIABLES: &[&str] = &[
    "CEREYAN_HOME",
    "CEREYAN_TOKEN",
    "CEREYAN_HOST",
    "CEREYAN_PORT",
    "CEREYAN_SOCKET",
    "CEREYAN_BASE_PATH",
    "CEREYAN_ENABLE_AUTH",
    "CEREYAN_AUTH_COOKIE",
    "CEREYAN_AUTH_SCOPE",
    "CEREYAN_LOGIN_URL",
    "CEREYAN_NO_BROWSER",
];

/// The shell's working directory: its name matches `PWD` but it holds no secret.
const NOT_SECRET: &[&str] = &["PWD", "OLDPWD"];

#[derive(Serialize, utoipa::ToSchema)]
pub struct Runtime {
    /// Python interpreter engines run with.
    pub python: String,
    pub python_version: Option<String>,
    pub platform: Option<String>,
    /// The served directory's `cereyan.toml`, when it has one.
    pub config_file: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ConfigEntry {
    pub table: String,
    pub key: String,
    /// The value in effect; null when nothing sets it or it is secret.
    #[schema(value_type = Object)]
    pub value: Value,
    /// `flag`, `env`, `app`, `toml`, `settings`, or `default`.
    pub source: String,
    /// The flag, variable, `app.serve()` argument, or table and key.
    pub source_name: Option<String>,
    /// True when the value is set and hidden.
    pub secret: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct EnvVariable {
    pub name: String,
    /// Null when hidden.
    pub value: Option<String>,
    pub hidden: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct Environment {
    pub runtime: Runtime,
    pub configuration: Vec<ConfigEntry>,
    /// The served directory's `cereyan.toml` with its secrets hidden.
    pub cereyan_toml: Option<String>,
    /// The server process environment, `CEREYAN_*` first.
    pub variables: Vec<EnvVariable>,
    /// Known `CEREYAN_*` variables that are not set.
    pub cereyan_unset: Vec<String>,
}

#[utoipa::path(get, path = "/api/settings/environment", responses((status = 200, body = Environment)))]
pub async fn get_environment(State(state): State<Arc<AppState>>) -> Json<Environment> {
    let config_file = state
        .config
        .served_dir
        .as_ref()
        .map(|dir| dir.join("cereyan.toml"))
        .filter(|path| path.is_file());
    let cereyan_toml = config_file
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| mask_toml(&text));
    let vars: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let cereyan_unset = KNOWN_VARIABLES
        .iter()
        .filter(|name| !vars.iter().any(|(k, _)| k == *name))
        .map(|name| name.to_string())
        .collect();
    Json(Environment {
        runtime: Runtime {
            python: state.config.python.clone(),
            python_version: state.config.python_version.clone(),
            platform: state.config.platform.clone(),
            config_file: config_file.map(|p| p.display().to_string()),
        },
        configuration: configuration(&state),
        cereyan_toml,
        variables: variables(vars, &secret_values(&state)),
        cereyan_unset,
    })
}

/// Values hidden wherever they appear: the API token and the email password.
fn secret_values(state: &AppState) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    out.extend(state.config.token.clone());
    out.extend(state.config.email.as_ref().and_then(|e| e.password.clone()));
    out.retain(|s| !s.is_empty());
    out
}

fn configuration(state: &AppState) -> Vec<ConfigEntry> {
    let sources = state
        .sources
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let c = &state.config;
    let mut out = Vec::new();
    let mut push = |table: &str, key: &str, value: Value, secret: bool| {
        let source = sources.get(&format!("{table}.{key}"));
        out.push(ConfigEntry {
            table: table.into(),
            key: key.into(),
            secret: secret && !value.is_null(),
            value: if secret { Value::Null } else { value },
            source: source.map_or_else(|| "default".into(), |s| s.source.clone()),
            source_name: source.and_then(|s| s.name.clone()),
        });
    };
    push("server", "host", json!(c.host), false);
    push("server", "port", json!(state.addr.port()), false);
    push("server", "base_path", json!(c.base_path), false);
    push("server", "token", json!(c.token), true);
    push(
        "server",
        "socket",
        json!(c.socket.as_ref().map(|p| p.display().to_string())),
        false,
    );
    push(
        "server",
        "max_engines",
        json!(state.supervisor.max_engines),
        false,
    );
    push("server", "engine_max_runs", json!(c.engine_max_runs), false);
    push(
        "server",
        "cancel_grace_secs",
        json!(c.cancel_grace_secs),
        false,
    );
    push("server", "open_browser", json!(c.open_browser), false);
    push(
        "server",
        "enable_auth",
        json!(state.authenticator.is_some()),
        false,
    );
    push("server", "auth_cookie", json!(c.auth_cookie), false);
    push("server", "auth_scope", json!(c.auth_scope), false);
    push("server", "login_url", json!(c.login_url), false);
    push("server", "allowed_hosts", json!(c.allowed_hosts), false);
    push(
        "server",
        "allow_unauthenticated",
        json!(c.allow_unauthenticated),
        false,
    );
    push("server", "mcp_read_only", json!(c.mcp_read_only), false);
    push("defaults", "catchup", json!(c.catchup_default), false);
    push(
        "defaults",
        "crash_retries",
        json!(state.crash_retries_default.load(Ordering::Relaxed)),
        false,
    );
    push(
        "defaults",
        "retain_days",
        json!(state.retain_days.load(Ordering::Relaxed)),
        false,
    );
    push("ui", "title", json!(state.title()), false);
    let mut totals: Vec<(String, f64)> = state.supervisor.resource_totals().into_iter().collect();
    totals.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, total) in totals {
        push("resources", &name, json!(total), false);
    }
    if let Some(e) = &c.email {
        push("email", "host", json!(e.host), false);
        push("email", "port", json!(e.port), false);
        push("email", "tls", json!(e.tls), false);
        push("email", "username", json!(e.username), false);
        push("email", "password", json!(e.password), true);
        push("email", "from", json!(e.from), false);
    }
    // With an authenticator and no configured token, the server made one up.
    if let Some(token) = out
        .iter_mut()
        .find(|e| e.key == "token" && e.table == "server")
    {
        if token.secret && token.source == "default" {
            token.source_name = Some("generated for this start".into());
        }
    }
    out
}

/// The process environment, `CEREYAN_*` first, with secrets hidden.
fn variables(mut vars: Vec<(String, String)>, secrets: &[String]) -> Vec<EnvVariable> {
    vars.sort_by(|(a, _), (b, _)| {
        (!a.starts_with("CEREYAN_"), a).cmp(&(!b.starts_with("CEREYAN_"), b))
    });
    vars.into_iter()
        .map(|(name, value)| {
            if is_hidden(&name, &value, secrets) {
                EnvVariable {
                    name,
                    value: None,
                    hidden: true,
                }
            } else {
                EnvVariable {
                    value: Some(mask_url_passwords(&value)),
                    name,
                    hidden: false,
                }
            }
        })
        .collect()
}

fn is_hidden(name: &str, value: &str, secrets: &[String]) -> bool {
    if name == "CEREYAN_TOKEN" || secrets.iter().any(|s| s == value) {
        return true;
    }
    if KNOWN_VARIABLES.contains(&name) || NOT_SECRET.contains(&name) {
        return false;
    }
    let upper = name.to_ascii_uppercase();
    SECRET_PARTS.iter().any(|part| upper.contains(part))
}

/// Replace the password of every `scheme://user:password@host` in `value`.
fn mask_url_passwords(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match (authority.rfind('@'), authority.find(':')) {
            (Some(at), Some(colon)) if colon < at => {
                out.push_str(&authority[..=colon]);
                out.push_str(URL_MASK);
                out.push_str(&authority[at..]);
            }
            _ => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// `cereyan.toml` with `[server] token` and `[email] password` replaced,
/// commented-out ones included; every other line is kept as written.
fn mask_toml(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut table = String::new();
    // The closing delimiter of a masked multi-line string still being skipped.
    let mut open_string: Option<&str> = None;
    for line in text.lines() {
        if let Some(delim) = open_string {
            if line.contains(delim) {
                open_string = None;
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            table = trimmed
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            out.push(line.to_string());
            continue;
        }
        let commented = trimmed.starts_with('#');
        let content = trimmed.trim_start_matches('#').trim_start();
        if let Some((key, value)) = content.split_once('=') {
            let key = key.trim();
            let bare = key.trim_matches(|c| c == '"' || c == '\'');
            let path = if table.is_empty() {
                bare.to_string()
            } else {
                format!("{table}.{bare}")
            };
            if path == "server.token" || path == "email.password" {
                let value = value.trim_start();
                if !commented {
                    open_string = ["\"\"\"", "'''"]
                        .into_iter()
                        .find(|d| value.starts_with(*d) && !value[3..].contains(*d));
                }
                let indent = &line[..line.len() - trimmed.len()];
                let hash = if commented { "# " } else { "" };
                out.push(format!("{indent}{hash}{key} = \"{TOML_MASK}\""));
                continue;
            }
        }
        out.push(line.to_string());
    }
    let mut masked = out.join("\n");
    if text.ends_with('\n') {
        masked.push('\n');
    }
    masked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> Vec<String> {
        vec!["tok-123".into()]
    }

    #[test]
    fn secret_by_name() {
        assert!(is_hidden("AWS_SECRET_ACCESS_KEY", "abc", &secrets()));
        assert!(is_hidden("SNOWFLAKE_PASSWORD", "abc", &secrets()));
        assert!(is_hidden("CEREYAN_TOKEN", "anything", &secrets()));
    }

    #[test]
    fn secret_by_value() {
        assert!(is_hidden("DEPLOY_NOTE", "tok-123", &secrets()));
    }

    #[test]
    fn ordinary_and_known_variables_shown() {
        assert!(!is_hidden("AWS_REGION", "eu-west-1", &secrets()));
        assert!(!is_hidden("CEREYAN_AUTH_SCOPE", "api", &secrets()));
        assert!(!is_hidden("CEREYAN_AUTH_COOKIE", "session", &secrets()));
        assert!(!is_hidden("PWD", "/home/ops", &secrets()));
    }

    #[test]
    fn url_passwords_replaced() {
        assert_eq!(
            mask_url_passwords("postgres://etl:pw@db.internal:5432/warehouse"),
            "postgres://etl:••••••@db.internal:5432/warehouse"
        );
        assert_eq!(
            mask_url_passwords("a=redis://:pw@cache b=https://user:x@h/p"),
            "a=redis://:••••••@cache b=https://user:••••••@h/p"
        );
        assert_eq!(
            mask_url_passwords("http://host:8080/x"),
            "http://host:8080/x"
        );
        assert_eq!(
            mask_url_passwords("https://user@host/x"),
            "https://user@host/x"
        );
        assert_eq!(mask_url_passwords("no url here"), "no url here");
    }

    #[test]
    fn variables_hide_and_sort() {
        let vars = vec![
            ("HOME".to_string(), "/home/ops".to_string()),
            ("AWS_SECRET_ACCESS_KEY".to_string(), "abc".to_string()),
            ("CEREYAN_HOME".to_string(), "/home/ops/.cereyan".to_string()),
        ];
        let out = variables(vars, &secrets());
        let names: Vec<&str> = out.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["CEREYAN_HOME", "AWS_SECRET_ACCESS_KEY", "HOME"]);
        assert!(out[1].hidden && out[1].value.is_none());
        assert_eq!(out[2].value.as_deref(), Some("/home/ops"));
    }

    #[test]
    fn toml_secrets_masked_and_comments_kept() {
        let text = "# production server\n[server]\nport = 4200\ntoken = \"s3cret\"\n# token = \"older\"\n\n[email]\nhost = \"smtp\"\npassword = \"\"\"\nline one\nline two\n\"\"\"\nfrom = \"a@b\"\n[other]\ntoken = \"kept\"\n";
        let masked = mask_toml(text);
        assert!(!masked.contains("s3cret"));
        assert!(!masked.contains("older"));
        assert!(!masked.contains("line one"));
        assert!(
            masked.contains("# production server\n[server]\nport = 4200\ntoken = \"••••••••\"\n")
        );
        assert!(masked.contains("# token = \"••••••••\""));
        assert!(masked.contains("password = \"••••••••\"\nfrom = \"a@b\""));
        assert!(masked.contains("[other]\ntoken = \"kept\"\n"));
    }

    #[test]
    fn dotted_keys_masked() {
        assert_eq!(
            mask_toml("server.token = \"x\"\n"),
            "server.token = \"••••••••\"\n"
        );
    }
}
