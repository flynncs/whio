use std::env;

use thiserror::Error;
use uuid::Uuid;
use whio_core::{
    database::Database,
    identity::{
        provisioning,
        secrets::CredentialKey,
        upstream::{self, UpstreamStoreError},
    },
};
use whio_navidrome_connector_api::{
    apis::{
        Error as ConnectorClientError, admin_api,
        configuration::Configuration as ConnectorConfiguration,
    },
    models::{ListUsersRequest, VerifyRequest},
};

#[derive(Debug, Error)]
enum CliError {
    #[error("{0}")]
    Usage(String),

    #[error("missing {0} (expected a flag, environment variable, or .env entry)")]
    MissingConfig(&'static str),

    #[error("cannot read .env: {0}")]
    Dotenv(#[from] std::io::Error),

    #[error("invalid WHIO_CREDENTIAL_KEY: {0}")]
    CredentialKey(#[from] whio_core::identity::secrets::SecretError),

    #[error(transparent)]
    Database(#[from] whio_core::database::DatabaseError),

    #[error(transparent)]
    Provisioning(#[from] provisioning::ProvisioningError),

    #[error(transparent)]
    Upstream(#[from] UpstreamStoreError),

    #[error("credential id is not a valid UUID")]
    InvalidCredentialId,

    #[error("upstream request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("upstream server url must be http(s), got `{0}`")]
    InvalidUpstreamUrl(String),
    #[error("upstream rejected the credentials")]
    UpstreamAuth,

    #[error("upstream user lacks admin privileges")]
    UpstreamNotAdmin,

    #[error("upstream response was not understood")]
    UnexpectedUpstreamResponse,
}

#[tokio::main]
async fn main() -> Result<(), CliError> {
    let dotenv = load_dotenv()?;
    let mut args = env::args().skip(1).peekable();

    let mut database_url_flag = None;
    let mut credential_key_flag = None;
    let mut connector_url_flag = None;
    while let Some(flag) = args.peek() {
        match flag.as_str() {
            "--database-url" => {
                args.next();
                database_url_flag =
                    Some(required_arg(&mut args, "--database-url requires a value")?);
            }
            "--credential-key" => {
                args.next();
                credential_key_flag = Some(required_arg(
                    &mut args,
                    "--credential-key requires a value",
                )?);
            }
            "--connector-url" => {
                args.next();
                connector_url_flag =
                    Some(required_arg(&mut args, "--connector-url requires a value")?);
            }
            _ => break,
        }
    }

    let Some(command) = args.next() else {
        print_help();
        return Ok(());
    };

    if command == "--help" || command == "-h" {
        print_help();
        return Ok(());
    }

    let operation = match command.as_str() {
        "create-user" => {
            let username = required_arg(&mut args, "create-user requires a username")?;
            let display_name = args.next().unwrap_or_else(|| username.clone());
            ensure_no_extra_args(&mut args)?;
            Operation::CreateUser {
                username,
                display_name,
            }
        }
        "add-server" => {
            let base_url = required_arg(&mut args, "add-server requires a base url")?;
            let admin_username = required_arg(&mut args, "add-server requires an admin username")?;
            ensure_no_extra_args(&mut args)?;
            Operation::AddServer {
                base_url,
                admin_username,
            }
        }
        "list-servers" => {
            ensure_no_extra_args(&mut args)?;
            Operation::ListServers
        }
        "link" => {
            let username = required_arg(&mut args, "link requires a whio username")?;
            let base_url = required_arg(&mut args, "link requires a server base url")?;
            let upstream_username = required_arg(&mut args, "link requires an upstream username")?;
            ensure_no_extra_args(&mut args)?;
            Operation::Link {
                username,
                base_url,
                upstream_username,
            }
        }
        "unlink" => {
            let username = required_arg(&mut args, "unlink requires a whio username")?;
            let base_url = required_arg(&mut args, "unlink requires a server base url")?;
            ensure_no_extra_args(&mut args)?;
            Operation::Unlink { username, base_url }
        }
        "relink" => {
            let username = required_arg(&mut args, "relink requires a whio username")?;
            let base_url = required_arg(&mut args, "relink requires a server base url")?;
            let upstream_username =
                required_arg(&mut args, "relink requires an upstream username")?;
            ensure_no_extra_args(&mut args)?;
            Operation::Relink {
                username,
                base_url,
                upstream_username,
            }
        }
        "mint-app-password" => {
            let username = required_arg(&mut args, "mint-app-password requires a username")?;
            let label = args.next().unwrap_or_else(|| "feishin".to_string());
            ensure_no_extra_args(&mut args)?;
            Operation::MintAppPassword { username, label }
        }
        "revoke" => {
            let raw_id = required_arg(&mut args, "revoke requires a credential id")?;
            ensure_no_extra_args(&mut args)?;
            let id = raw_id.parse().map_err(|_| CliError::InvalidCredentialId)?;
            Operation::Revoke { id }
        }
        _ => {
            return Err(CliError::Usage(format!(
                "unknown command `{command}` (try --help)"
            )));
        }
    };

    let database_url = resolve_config(&dotenv, database_url_flag, "WHIO_DATABASE_URL")?;
    let database = Database::connect(&database_url).await?;
    match operation {
        Operation::CreateUser {
            username,
            display_name,
        } => {
            let id = provisioning::create_user(&database.pool(), &username, &display_name).await?;
            println!("created user {username} ({id})");
        }
        Operation::MintAppPassword { username, label } => {
            let raw_key = resolve_config(&dotenv, credential_key_flag, "WHIO_CREDENTIAL_KEY")?;
            let key = CredentialKey::from_base64(&raw_key)?;
            let (id, secret) =
                provisioning::mint_app_password(&database.pool(), &key, &username, &label).await?;
            println!("credential id: {id}");
            println!("username: {username}");
            println!("password: {secret}");
            eprintln!("save this password");
        }
        Operation::Revoke { id } => {
            if provisioning::revoke_credential(&database.pool(), id).await? {
                println!("revoked credential {id}");
            } else {
                return Err(CliError::Usage(format!(
                    "active credential {id} was not found"
                )));
            }
        }
        Operation::AddServer {
            base_url,
            admin_username,
        } => {
            let raw_key = resolve_config(&dotenv, credential_key_flag, "WHIO_CREDENTIAL_KEY")?;
            let key = CredentialKey::from_base64(&raw_key)?;
            let connector = connector_config(&dotenv, connector_url_flag)?;
            let base_url = base_url.trim_end_matches('/').to_owned();
            let admin_password = read_password("navidrome admin password")?;
            let discovered =
                list_upstream_users(&connector, &base_url, &admin_username, &admin_password)
                    .await?;
            let id = upstream::add_server(
                &database.pool(),
                &key,
                &base_url,
                &admin_username,
                &admin_password,
            )
            .await?;
            println!("registered upstream server {base_url} ({id})");
            println!("upstream users: {}", discovered.join(", "));
        }
        Operation::ListServers => {
            for server in upstream::list_servers(&database.pool()).await? {
                println!(
                    "{} {} admin={} enabled={}",
                    server.id, server.base_url, server.admin_username, server.enabled
                );
            }
        }
        Operation::Link {
            username,
            base_url,
            upstream_username,
        } => {
            let raw_key = resolve_config(&dotenv, credential_key_flag, "WHIO_CREDENTIAL_KEY")?;
            let key = CredentialKey::from_base64(&raw_key)?;
            let connector = connector_config(&dotenv, connector_url_flag)?;
            let base_url = base_url.trim_end_matches('/').to_owned();
            let upstream_password = read_password("navidrome user password")?;
            verify_upstream_credentials(
                &connector,
                &base_url,
                &upstream_username,
                &upstream_password,
            )
            .await?;
            match upstream::store_link(
                &database.pool(),
                &key,
                &username,
                &base_url,
                &upstream_username,
                &upstream_password,
            )
            .await
            {
                Ok(id) => println!("linked {username} to {upstream_username} on {base_url} ({id})"),
                Err(UpstreamStoreError::AlreadyLinked) => {
                    return Err(CliError::Usage(format!(
                        "{username} is already linked to {base_url} (use relink to update)"
                    )));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Operation::Relink {
            username,
            base_url,
            upstream_username,
        } => {
            let raw_key = resolve_config(&dotenv, credential_key_flag, "WHIO_CREDENTIAL_KEY")?;
            let key = CredentialKey::from_base64(&raw_key)?;
            let connector = connector_config(&dotenv, connector_url_flag)?;
            let base_url = base_url.trim_end_matches('/').to_owned();
            let upstream_password = read_password("navidrome user password")?;
            verify_upstream_credentials(
                &connector,
                &base_url,
                &upstream_username,
                &upstream_password,
            )
            .await?;
            if upstream::update_link(
                &database.pool(),
                &key,
                &username,
                &base_url,
                &upstream_username,
                &upstream_password,
            )
            .await?
            {
                println!("relinked {username} to {upstream_username} on {base_url}");
            } else {
                return Err(CliError::Usage(format!(
                    "{username} is not linked to {base_url}"
                )));
            }
        }
        Operation::Unlink { username, base_url } => {
            let base_url = base_url.trim_end_matches('/').to_owned();
            if upstream::remove_link(&database.pool(), &username, &base_url).await? {
                println!("unlinked {username} from {base_url}");
            } else {
                return Err(CliError::Usage(format!(
                    "{username} is not linked to {base_url}"
                )));
            }
        }
    }

    Ok(())
}

enum Operation {
    CreateUser {
        username: String,
        display_name: String,
    },
    MintAppPassword {
        username: String,
        label: String,
    },
    Revoke {
        id: Uuid,
    },
    AddServer {
        base_url: String,
        admin_username: String,
    },
    ListServers,
    Link {
        username: String,
        base_url: String,
        upstream_username: String,
    },
    Unlink {
        username: String,
        base_url: String,
    },
    Relink {
        username: String,
        base_url: String,
        upstream_username: String,
    },
}

fn load_dotenv() -> Result<Vec<(String, String)>, CliError> {
    let content = match std::fs::read_to_string(".env") {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(CliError::Dotenv(error)),
    };
    let mut entries = Vec::new();
    for raw in content.lines() {
        let line = raw
            .trim()
            .strip_prefix("export ")
            .unwrap_or(raw.trim())
            .trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        entries.push((name.to_string(), value.trim().to_string()));
    }
    Ok(entries)
}

fn resolve_config(
    dotenv: &[(String, String)],
    flag: Option<String>,
    name: &'static str,
) -> Result<String, CliError> {
    if let Some(value) = flag {
        return Ok(value);
    }
    if let Ok(value) = env::var(name) {
        return Ok(value);
    }
    if let Some(value) = dotenv
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
    {
        return Ok(value);
    }
    Err(CliError::MissingConfig(name))
}

fn required_arg(
    args: &mut impl Iterator<Item = String>,
    message: &str,
) -> Result<String, CliError> {
    args.next()
        .ok_or_else(|| CliError::Usage(message.to_string()))
}

fn ensure_no_extra_args(args: &mut impl Iterator<Item = String>) -> Result<(), CliError> {
    if let Some(extra) = args.next() {
        return Err(CliError::Usage(format!("unexpected argument `{extra}`")));
    }
    Ok(())
}

fn print_help() {
    println!(
        "whio-cli\n\n\
         Usage:\n\
         \\twhio-cli [--database-url <url>] [--credential-key <key>] create-user <username> [display-name]\n\
         \\twhio-cli [--database-url <url>] [--credential-key <key>] mint-app-password <username> [label]\n\
         \\twhio-cli [--database-url <url>] revoke <credential-id>\n\
         \\twhio-cli [--database-url <url>] [--credential-key <key>] add-server <base-url> <admin-username>\n\
         \\twhio-cli [--database-url <url>] list-servers\n\
         \\twhio-cli [--database-url <url>] [--credential-key <key>] [--connector-url <url>] link <username> <base-url> <upstream-username>\n\
         \\twhio-cli [--database-url <url>] [--credential-key <key>] [--connector-url <url>] relink <username> <base-url> <upstream-username>\n\
         \\twhio-cli [--database-url <url>] unlink <username> <base-url>\n\n\
         Configuration (first match wins):\n\
         \\t--database-url / --credential-key / --connector-url flags\n\
         \\tWHIO_UPSTREAM_CONNECTOR_URL environment variable or .env entry is required for add-server, link and relink\n\
         \\tWHIO_DATABASE_URL / WHIO_CREDENTIAL_KEY environment variables\n\
         \t.env in the current directory"
    );
}

fn read_password(prompt: &str) -> Result<String, CliError> {
    use std::io::{self, BufRead, Write};

    eprint!("{prompt}: ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

fn connector_config(
    dotenv: &[(String, String)],
    flag: Option<String>,
) -> Result<ConnectorConfiguration, CliError> {
    let raw = resolve_config(dotenv, flag, "WHIO_UPSTREAM_CONNECTOR_URL")?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(CliError::Http)?;
    Ok(ConnectorConfiguration {
        base_path: raw.trim_end_matches('/').to_owned(),
        client,
        ..ConnectorConfiguration::default()
    })
}

fn map_connector_error<E>(error: ConnectorClientError<E>, base_url: &str) -> CliError {
    match error {
        ConnectorClientError::ResponseError(response) => match response.status {
            reqwest::StatusCode::BAD_REQUEST => CliError::InvalidUpstreamUrl(base_url.to_owned()),
            reqwest::StatusCode::UNAUTHORIZED => CliError::UpstreamAuth,
            reqwest::StatusCode::FORBIDDEN => CliError::UpstreamNotAdmin,
            _ => CliError::UnexpectedUpstreamResponse,
        },
        ConnectorClientError::Reqwest(error) => CliError::Http(error),
        _ => CliError::UnexpectedUpstreamResponse,
    }
}

async fn verify_upstream_credentials(
    connector: &ConnectorConfiguration,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<(), CliError> {
    let request = VerifyRequest::new(
        base_url.to_owned(),
        username.to_owned(),
        password.to_owned(),
    );
    admin_api::verify_upstream_credentials(connector, request, None)
        .await
        .map(|_| ())
        .map_err(|error| map_connector_error(error, base_url))
}

async fn list_upstream_users(
    connector: &ConnectorConfiguration,
    base_url: &str,
    admin_username: &str,
    admin_password: &str,
) -> Result<Vec<String>, CliError> {
    let request = ListUsersRequest::new(
        base_url.to_owned(),
        admin_username.to_owned(),
        admin_password.to_owned(),
    );
    admin_api::list_upstream_users(connector, request, None)
        .await
        .map(|response| response.usernames)
        .map_err(|error| map_connector_error(error, base_url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use whio_navidrome_connector_api::apis::ResponseContent;

    fn response_error(
        status: reqwest::StatusCode,
    ) -> ConnectorClientError<admin_api::ListUpstreamUsersError> {
        ConnectorClientError::ResponseError(ResponseContent {
            status,
            content: String::new(),
            entity: None,
        })
    }

    #[test]
    fn connector_statuses_map_to_cli_errors() {
        assert!(matches!(
            map_connector_error(
                response_error(reqwest::StatusCode::UNAUTHORIZED),
                "https://x"
            ),
            CliError::UpstreamAuth
        ));
        assert!(matches!(
            map_connector_error(response_error(reqwest::StatusCode::FORBIDDEN), "https://x"),
            CliError::UpstreamNotAdmin
        ));
        assert!(matches!(
            map_connector_error(
                response_error(reqwest::StatusCode::BAD_REQUEST),
                "https://x"
            ),
            CliError::InvalidUpstreamUrl(_)
        ));
        assert!(matches!(
            map_connector_error(
                response_error(reqwest::StatusCode::BAD_GATEWAY),
                "https://x"
            ),
            CliError::UnexpectedUpstreamResponse
        ));
    }
}
