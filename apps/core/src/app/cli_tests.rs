use super::*;

#[test]
fn an_unknown_prune_flag_is_rejected() {
    let result = parse_prune_provider_duplicates(&mut ["--wrong".to_owned()].into_iter());
    assert!(matches!(result, Err(CliError::Usage(_))));
}
