use uuid::Uuid;

use crate::features::identity::{
    provisioning::{ProvisioningError, ProvisioningService},
    secrets::CredentialKey,
};

pub(crate) async fn create_user(
    service: &ProvisioningService,
    username: &str,
    display_name: &str,
) -> Result<(), ProvisioningError> {
    let id = service.create_user(username, display_name).await?;
    println!("created user {username} ({id})");
    Ok(())
}

pub(crate) async fn mint_app_password(
    service: &ProvisioningService,
    key: &CredentialKey,
    username: &str,
    label: &str,
) -> Result<(), ProvisioningError> {
    let (id, secret) = service.mint_app_password(key, username, label).await?;
    println!("credential id: {id}");
    println!("username: {username}");
    println!("password: {secret}");
    eprintln!("save this password");
    Ok(())
}

pub(crate) async fn revoke(
    service: &ProvisioningService,
    id: Uuid,
) -> Result<bool, ProvisioningError> {
    let revoked = service.revoke_credential(id).await?;
    if revoked {
        println!("revoked credential {id}");
    }
    Ok(revoked)
}

pub(crate) async fn assign_admin(
    service: &ProvisioningService,
    username: &str,
) -> Result<(), ProvisioningError> {
    let role = service.assign_admin(username).await?;
    println!("{username}: {}", role.as_str());
    Ok(())
}
