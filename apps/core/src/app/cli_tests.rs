use super::*;

#[cfg(feature = "postgres-integration")]
#[sqlx::test]
async fn disabled_acquisition_provides_no_prune_protection_without_reading_storage(
    pool: sqlx::PgPool,
) {
    pool.close().await;
    let vars = crate::platform::config::Vars::from([(
        "WHIO_ACQUISITION_ENABLED".to_owned(),
        "false".to_owned(),
    )]);
    assert!(
        prune_protected_origins(&vars, pool.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        prune_protected_origins(&crate::platform::config::Vars::new(), pool)
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cli_configuration_errors_do_not_echo_credentials() {
    let error = CliError::from(crate::platform::config::ConfigError::Invalid {
        name: "WHIO_CREDENTIAL_KEY",
        value: "private-key".to_owned(),
        reason: "must be valid base64",
    });
    assert_eq!(
        error.to_string(),
        "invalid WHIO_CREDENTIAL_KEY: must be valid base64"
    );
}

#[test]
fn an_unknown_prune_flag_is_rejected() {
    let result = parse_prune_provider_duplicates(&mut ["--wrong".to_owned()].into_iter());
    assert!(matches!(result, Err(CliError::Usage(_))));
}
