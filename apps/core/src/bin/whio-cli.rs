use std::env;

use thiserror::Error;
use uuid::Uuid;
use whio_core::{
    database::Database,
    identity::{provisioning, secrets::CredentialKey},
};

#[derive(Debug, Error)]
enum CliError {
    #[error("{0}")]
    Usage(String),

    #[error("missing required environment variable {0}")]
    MissingEnvironment(&'static str),

    #[error("invalid WHIO_CREDENTIAL_KEY: {0}")]
    CredentialKey(#[from] whio_core::identity::secrets::SecretError),

    #[error(transparent)]
    Database(#[from] whio_core::database::DatabaseError),

    #[error(transparent)]
    Provisioning(#[from] provisioning::ProvisioningError),

    #[error("credential id is not a valid UUID")]
    InvalidCredentialId,
}

#[tokio::main]
async fn main() -> Result<(), CliError> {
    let mut args = env::args().skip(1);
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

    let database_url = env::var("WHIO_DATABASE_URL")
        .map_err(|_| CliError::MissingEnvironment("WHIO_DATABASE_URL"))?;
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
            let raw_key = env::var("WHIO_CREDENTIAL_KEY")
                .map_err(|_| CliError::MissingEnvironment("WHIO_CREDENTIAL_KEY"))?;
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
         \\twhio-cli create-user <username> [display-name]\n\
         \\twhio-cli mint-app-password <username> [label]\n\
         \\twhio-cli revoke <credential-id>\n\n\
         Environment:\n\
         \\tWHIO_DATABASE_URL\n\
         \\tWHIO_CREDENTIAL_KEY (required for mint-app-password)"
    );
}
