use std::env;

use thiserror::Error;
use uuid::Uuid;
use whio_core::{
    database::Database,
    identity::{provisioning, secrets::CredentialKey},
    tracks::{
        DuplicateCandidate, DuplicateReport, PostgresTrackRepository, SplitArtist, SplitArtistRow,
    },
};

const DEFAULT_DUPLICATE_LIMIT: u32 = 50;

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

    #[error("credential id is not a valid UUID")]
    InvalidCredentialId,

    #[error(transparent)]
    Repository(#[from] whio_core::tracks::TrackRepositoryError),
}

#[tokio::main]
async fn main() -> Result<(), CliError> {
    let dotenv = load_dotenv()?;
    let mut args = env::args().skip(1).peekable();

    let mut database_url_flag = None;
    let mut credential_key_flag = None;
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
        "duplicates" => {
            let limit = match args.next() {
                Some(raw) => raw.parse().map_err(|_| {
                    CliError::Usage(format!("duplicates limit `{raw}` is not a number"))
                })?,
                None => DEFAULT_DUPLICATE_LIMIT,
            };
            ensure_no_extra_args(&mut args)?;
            Operation::Duplicates { limit }
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
        Operation::Duplicates { limit } => {
            let repository = PostgresTrackRepository::new(database.pool());
            let candidates = repository.duplicate_candidates(limit).await?;
            print_duplicates(&candidates);

            let split = repository.split_artists(limit).await?;
            print_split_artists(&split);
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
    Duplicates {
        limit: u32,
    },
}

fn print_duplicates(candidates: &[DuplicateCandidate]) {
    if candidates.is_empty() {
        println!("no track has more than one local file");
        return;
    }

    let wasted: u64 = candidates
        .iter()
        .map(DuplicateCandidate::wasted_bytes)
        .sum();

    println!(
        "{} tracks hold more than one local file; the copies a keep-rule would retire total {}",
        candidates.len(),
        human_bytes(wasted)
    );

    for candidate in candidates {
        println!(
            "\n{} — {}",
            candidate.artists().join("; "),
            candidate.title()
        );

        let keep = candidate.keep().map(|(source, reason)| {
            println!("  reason: {}", reason.as_str());
            source.source().clone()
        });

        for source in candidate.sources() {
            let marker = if Some(source.source()) == keep.as_ref() {
                "keep  "
            } else {
                "retire"
            };

            println!(
                "  {marker}  {:>10}  {:<6}  {}",
                human_bytes(source.size_bytes()),
                source.suffix().unwrap_or("-"),
                source.path()
            );
        }
    }
}

fn print_split_artists(artists: &[SplitArtist]) {
    if artists.is_empty() {
        return;
    }

    println!(
        "\n{} artist names are held by more than one row",
        artists.len()
    );

    for artist in artists {
        let header = artist
            .rows()
            .first()
            .map_or(artist.name(), SplitArtistRow::name);

        println!("\n{header}");

        for row in artist.rows() {
            println!(
                "  {}  {:<4}  {} tracks, {} albums",
                row.artist_id().as_uuid(),
                if row.is_identified() { "id" } else { "none" },
                row.track_count(),
                row.album_count()
            );
        }
    }

    println!("\nA-046 keeps these rows apart on purpose; merging them is a decision, not a fix.");
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
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
         \\twhio-cli [--database-url <url>] duplicates [limit]\n\n\
         Configuration (first match wins):\n\
         \\t--database-url / --credential-key flags\n\
         \\tWHIO_DATABASE_URL / WHIO_CREDENTIAL_KEY environment variables\n\
         \t.env in the current directory"
    );
}
