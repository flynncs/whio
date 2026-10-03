use crate::features::metadata::domain::{
    Attribution, BytesState, FactSource, FieldId, FieldValue, MatchBasis, StoredFact,
};
use crate::features::metadata::licences::{LicenceTerms, licence_terms};
use crate::features::metadata::selection::{
    COMMONS, FieldOrder, LASTFM, LISTENBRAINZ, LOCAL, MUSICBRAINZ, Selection, THEAUDIODB,
    WIKIPEDIA, YOUTUBE_MUSIC, select,
};

fn terms(id: &str) -> LicenceTerms {
    licence_terms(id, 1).expect("a registered licence")
}

fn fact(field: FieldId, provider_id: &str, external_id: &str, value: &str) -> StoredFact {
    StoredFact {
        field,
        source: FactSource {
            provider_id: provider_id.to_owned(),
            external_id: external_id.to_owned(),
            url: None,
            match_basis: MatchBasis::Identifier,
        },
        value: FieldValue::Text(value.to_owned()),
        licence: terms("musicbrainz"),
        attribution: None,
        source_url: None,
        operation: "artist-display.v1".to_owned(),
        terms_basis: None,
        uncertainty: false,
        image: None,
        metric: None,
        rank: None,
        score: None,
        bytes: None,
        stored_bytes: value.len() as u64,
        fetched_at: chrono::Utc::now(),
    }
}

fn order(entries: &[(FieldId, &[&str])]) -> FieldOrder {
    FieldOrder::from_entries(
        entries
            .iter()
            .map(|(field, sources)| {
                (
                    *field,
                    sources.iter().map(|source| (*source).to_owned()).collect(),
                )
            })
            .collect::<Vec<_>>(),
    )
}

#[test]
fn the_configured_order_picks_the_winner_and_records_every_later_value() {
    let facts = vec![
        fact(FieldId::BiographyContent, THEAUDIODB, "audiodb-1", "second"),
        fact(FieldId::BiographyContent, MUSICBRAINZ, "mbid-1", "third"),
        fact(
            FieldId::BiographyContent,
            YOUTUBE_MUSIC,
            "channel-1",
            "first",
        ),
    ];
    let order = order(&[(
        FieldId::BiographyContent,
        &[YOUTUBE_MUSIC, THEAUDIODB, MUSICBRAINZ],
    )]);

    let Selection {
        winner,
        disagreements,
    } = select(&facts, &order);

    assert_eq!(
        winner.map(|fact| (fact.source.provider_id.as_str(), fact.value.clone())),
        Some((YOUTUBE_MUSIC, FieldValue::Text("first".to_owned())))
    );
    let losers = disagreements
        .iter()
        .map(|fact| fact.source.provider_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(losers, vec![THEAUDIODB, MUSICBRAINZ]);
}

#[test]
fn the_order_is_the_configuration_and_not_the_licence_tier() {
    let facts = vec![
        fact(FieldId::ImagePortrait, COMMONS, "file-1", "commons"),
        fact(FieldId::ImagePortrait, THEAUDIODB, "audiodb-1", "audiodb"),
    ];
    let reversed = order(&[(FieldId::ImagePortrait, &[THEAUDIODB, COMMONS])]);
    let commons_first = order(&[(FieldId::ImagePortrait, &[COMMONS, THEAUDIODB])]);

    let winner = |order: &FieldOrder| {
        select(&facts, order)
            .winner
            .map(|fact| fact.source.provider_id.clone())
    };

    assert_eq!(winner(&reversed), Some(THEAUDIODB.to_owned()));
    assert_eq!(winner(&commons_first), Some(COMMONS.to_owned()));
}

#[test]
fn one_configured_source_present_is_a_winner_without_a_disagreement() {
    let facts = vec![fact(FieldId::Tag, LISTENBRAINZ, "lb-1", "electronic")];
    let order = order(&[(FieldId::Tag, &[MUSICBRAINZ, LISTENBRAINZ])]);

    let selection = select(&facts, &order);

    assert_eq!(
        selection
            .winner
            .map(|fact| fact.source.provider_id.as_str()),
        Some(LISTENBRAINZ)
    );
    assert!(!selection.contested());
}

#[test]
fn no_facts_selects_nothing() {
    let order = FieldOrder::defaults();
    let selection = select(&[], &order);

    assert!(selection.winner.is_none());
    assert!(selection.disagreements.is_empty());
    assert!(!selection.contested());
}

#[test]
fn an_empty_configured_order_selects_nothing_from_present_facts() {
    let facts = vec![
        fact(
            FieldId::BiographyContent,
            YOUTUBE_MUSIC,
            "channel-1",
            "text",
        ),
        fact(FieldId::BiographyContent, LASTFM, "lastfm-1", "other"),
    ];
    let order = order(&[(FieldId::BiographyContent, &[])]);

    let selection = select(&facts, &order);

    assert!(selection.winner.is_none());
    assert!(selection.disagreements.is_empty());
}

#[test]
fn a_field_missing_from_the_order_has_no_configured_sources() {
    let order = FieldOrder::new();

    assert!(order.sources(FieldId::BiographyContent).is_empty());
    assert!(order.sources(FieldId::ImagePortrait).is_empty());
}

#[test]
fn a_source_absent_from_the_order_never_wins() {
    let facts = vec![
        fact(
            FieldId::BiographyContent,
            LASTFM,
            "lastfm-1",
            "unconfigured",
        ),
        fact(
            FieldId::BiographyContent,
            THEAUDIODB,
            "audiodb-1",
            "configured",
        ),
    ];
    let order = order(&[(FieldId::BiographyContent, &[THEAUDIODB])]);

    let selection = select(&facts, &order);

    assert_eq!(
        selection
            .winner
            .map(|fact| fact.source.provider_id.as_str()),
        Some(THEAUDIODB)
    );
    assert!(selection.disagreements.is_empty());
}

#[test]
fn a_fact_set_with_no_configured_source_selects_nothing() {
    let facts = vec![
        fact(FieldId::BiographyContent, WIKIPEDIA, "wiki-2", "b"),
        fact(FieldId::BiographyContent, WIKIPEDIA, "wiki-1", "a"),
        fact(FieldId::BiographyContent, COMMONS, "commons-1", "c"),
    ];
    let order = order(&[(FieldId::BiographyContent, &[THEAUDIODB])]);

    let selection = select(&facts, &order);

    assert!(
        selection.winner.is_none(),
        "an unconfigured source is never promoted to a winner"
    );
    assert!(selection.disagreements.is_empty());
}

#[test]
fn the_default_order_covers_every_field_with_the_configured_first_source() {
    let order = FieldOrder::defaults();

    for (field, first) in [
        (FieldId::IdentityName, MUSICBRAINZ),
        (FieldId::IdentityMbid, LOCAL),
        (FieldId::IdentityDescription, WIKIPEDIA),
        (FieldId::BiographyContent, YOUTUBE_MUSIC),
        (FieldId::BiographySummary, WIKIPEDIA),
        (FieldId::ImagePortrait, THEAUDIODB),
        (FieldId::ImageWide, THEAUDIODB),
        (FieldId::PageUrl, MUSICBRAINZ),
        (FieldId::Tag, MUSICBRAINZ),
        (FieldId::Similar, YOUTUBE_MUSIC),
        (FieldId::TopTracks, YOUTUBE_MUSIC),
        (FieldId::MetricListeners, LASTFM),
        (FieldId::MetricPlays, LASTFM),
        (FieldId::MetricListen, LISTENBRAINZ),
        (FieldId::MetricListener, LISTENBRAINZ),
    ] {
        assert_eq!(
            order.sources(field).first().map(String::as_str),
            Some(first),
            "{field:?}"
        );
    }

    assert!(
        order.sources(FieldId::BiographyLanguage).is_empty(),
        "the language follows whichever biography won"
    );
    assert!(
        !order
            .sources(FieldId::ImagePortrait)
            .contains(&YOUTUBE_MUSIC.to_owned()),
        "ytm supplies no square portrait"
    );
}

#[test]
fn a_default_order_winner_is_the_first_configured_source_that_answered() {
    let facts = vec![
        fact(FieldId::BiographyContent, WIKIPEDIA, "wiki-1", "wikipedia"),
        fact(FieldId::BiographyContent, LASTFM, "lastfm-1", "lastfm"),
    ];

    let selection = select(&facts, &FieldOrder::defaults());

    assert_eq!(
        selection
            .winner
            .map(|fact| fact.source.provider_id.as_str()),
        Some(WIKIPEDIA)
    );
    assert_eq!(
        selection
            .disagreements
            .iter()
            .map(|fact| fact.source.provider_id.as_str())
            .collect::<Vec<_>>(),
        vec![LASTFM]
    );
}

#[test]
fn a_stored_reference_fact_keeps_its_bytes_state_through_selection() {
    let mut portrait = fact(FieldId::ImagePortrait, COMMONS, "file-1", "ref");
    portrait.value = FieldValue::Reference("https://commons.example/f".to_owned());
    portrait.bytes = Some(BytesState::Servable);
    portrait.attribution = Some(Attribution {
        text: "CC BY-SA 2.0".to_owned(),
        url: Some("https://commons.example/f".to_owned()),
    });
    let facts = vec![portrait];

    let selection = select(&facts, &FieldOrder::defaults());

    assert_eq!(
        selection.winner.and_then(|fact| fact.bytes),
        Some(BytesState::Servable)
    );
}
