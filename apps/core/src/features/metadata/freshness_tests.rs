use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::features::metadata::domain::{
    AbsenceReason, AttemptError, DESCRIPTIVE_TTL, DISCOVERY_TTL, FactSource, FieldId, FieldValue,
    Freshness, MatchBasis, NEGATIVE_TTL, RELATIONSHIP_TTL, StoredAttempt, StoredFact,
};
use crate::features::metadata::freshness::{
    FAILURE_FLOOR, FixedJitter, JITTER_PERCENT, NoJitter, absence, freshness, is_due, jittered,
    ttl_for,
};
use crate::features::metadata::licences::licence_terms;

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("a valid instant")
}

fn fact(fetched_at: DateTime<Utc>) -> StoredFact {
    StoredFact {
        field: FieldId::BiographyContent,
        source: FactSource {
            provider_id: "theaudiodb".to_owned(),
            external_id: "audiodb-1".to_owned(),
            url: None,
            match_basis: MatchBasis::Identifier,
        },
        value: FieldValue::Text("a biography".to_owned()),
        licence: licence_terms("theaudiodb", 1).expect("a registered licence"),
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
        stored_bytes: 11,
        fetched_at,
    }
}

fn attempt(
    attempted_at: DateTime<Utc>,
    error: Option<&str>,
    fetched_at: Option<DateTime<Utc>>,
) -> StoredAttempt {
    StoredAttempt {
        field: FieldId::BiographyContent,
        source_provider_id: "theaudiodb".to_owned(),
        fetched_at,
        attempted_at,
        error: error.map(|message| AttemptError {
            class: "provider_unavailable".to_owned(),
            message: message.to_owned(),
        }),
    }
}

#[test]
fn the_four_ttl_families_are_the_d6_values() {
    assert_eq!(
        ttl_for("biography.content").expect("a descriptive field"),
        Duration::from_secs(14 * 24 * 60 * 60)
    );
    assert_eq!(
        ttl_for("biography.summary").expect("a descriptive field"),
        DESCRIPTIVE_TTL
    );
    assert_eq!(
        ttl_for("identity.description").expect("a descriptive field"),
        DESCRIPTIVE_TTL
    );
    assert_eq!(
        ttl_for("image.portrait").expect("a descriptive field"),
        DESCRIPTIVE_TTL
    );
    assert_eq!(
        ttl_for("page.url").expect("a descriptive field"),
        DESCRIPTIVE_TTL
    );
    assert_eq!(
        ttl_for("tag").expect("a descriptive field"),
        DESCRIPTIVE_TTL
    );

    assert_eq!(
        ttl_for("identity.mbid").expect("a relationship field"),
        Duration::from_secs(90 * 24 * 60 * 60)
    );
    assert_eq!(
        ttl_for("identity.name").expect("a relationship field"),
        RELATIONSHIP_TTL
    );

    for field in [
        "similar",
        "top_tracks",
        "metric.listeners",
        "metric.plays",
        "metric.listen",
        "metric.listener",
    ] {
        assert_eq!(
            ttl_for(field).expect("a discovery field"),
            Duration::from_secs(24 * 60 * 60),
            "{field}"
        );
        assert_eq!(ttl_for(field).expect("a discovery field"), DISCOVERY_TTL);
    }

    assert_ne!(DESCRIPTIVE_TTL, RELATIONSHIP_TTL);
    assert_ne!(DISCOVERY_TTL, NEGATIVE_TTL);
}

#[test]
fn an_unknown_field_has_no_ttl() {
    let unknown = ttl_for("biography.made_up").expect_err("an unknown field");

    assert_eq!(unknown.name(), "biography.made_up");
    assert!(ttl_for("").is_err());
    assert!(ttl_for("Biography.Content").is_err());
}

#[test]
fn the_negative_ttl_is_seven_days() {
    assert_eq!(NEGATIVE_TTL, Duration::from_secs(7 * 24 * 60 * 60));
    assert!(NEGATIVE_TTL < DESCRIPTIVE_TTL);
    assert!(NEGATIVE_TTL > DISCOVERY_TTL);
}

#[test]
fn an_untouched_field_is_unrequested() {
    assert_eq!(
        freshness(None, None, DESCRIPTIVE_TTL, at(0)),
        Freshness::Unrequested
    );
    assert!(is_due(&[], &[], DESCRIPTIVE_TTL, &NoJitter, at(0)));
    assert_eq!(
        absence(&[], &[], DESCRIPTIVE_TTL, at(0)),
        Some(AbsenceReason::Unrequested)
    );
}

#[test]
fn a_recent_success_is_fresh_and_not_due() {
    let fetched = at(0);
    let ttl = DESCRIPTIVE_TTL;

    assert_eq!(
        freshness(Some(fetched), None, ttl, at(60)),
        Freshness::Fresh
    );
    assert!(!is_due(&[fact(fetched)], &[], ttl, &NoJitter, at(60)));
}

#[test]
fn a_success_past_its_ttl_is_stale_and_due() {
    let fetched = at(0);
    let ttl = DISCOVERY_TTL;
    let now = at(24 * 60 * 60 + 1);

    assert_eq!(freshness(Some(fetched), None, ttl, now), Freshness::Stale);
    assert!(is_due(&[fact(fetched)], &[], ttl, &NoJitter, now));
}

#[test]
fn a_successful_empty_uses_the_negative_ttl_and_reports_provider_reported_none() {
    let reported = at(0);
    let attempts = vec![attempt(reported, None, Some(reported))];

    assert_eq!(
        freshness(None, attempts.first(), DESCRIPTIVE_TTL, at(60)),
        Freshness::Fresh
    );
    assert_eq!(
        absence(&[], &attempts, DESCRIPTIVE_TTL, at(60)),
        Some(AbsenceReason::ProviderReportedNone)
    );
    assert!(!is_due(&[], &attempts, DESCRIPTIVE_TTL, &NoJitter, at(60)));
}

#[test]
fn a_successful_empty_past_the_negative_ttl_is_stale_absent_and_due() {
    let reported = at(0);
    let attempts = vec![attempt(reported, None, Some(reported))];
    let now = at(7 * 24 * 60 * 60);

    assert_eq!(
        absence(&[], &attempts, DESCRIPTIVE_TTL, now),
        Some(AbsenceReason::StaleAbsent)
    );
    assert_eq!(
        freshness(None, attempts.first(), DESCRIPTIVE_TTL, now),
        Freshness::Stale
    );
    assert!(is_due(&[], &attempts, DESCRIPTIVE_TTL, &NoJitter, now));
    assert!(
        !is_due(
            &[],
            &attempts,
            DESCRIPTIVE_TTL,
            &NoJitter,
            at(7 * 24 * 60 * 60 - 1)
        ),
        "a successful empty is not refreshed before the negative ttl"
    );
}

#[test]
fn a_failed_attempt_is_failed_with_no_absence() {
    let attempted = at(0);
    let attempts = vec![attempt(attempted, Some("provider answered 404"), None)];

    assert_eq!(
        freshness(None, attempts.first(), DESCRIPTIVE_TTL, at(60)),
        Freshness::Failed("provider answered 404".to_owned())
    );
    assert_eq!(
        absence(&[], &attempts, DESCRIPTIVE_TTL, at(60)),
        None,
        "a failure is never an absence reason"
    );
}

#[test]
fn the_failure_floor_delays_a_retry_for_one_hour() {
    let attempted = at(0);
    let attempts = vec![attempt(attempted, Some("provider answered 404"), None)];

    assert_eq!(FAILURE_FLOOR, Duration::from_secs(60 * 60));
    assert!(
        !is_due(&[], &attempts, DESCRIPTIVE_TTL, &NoJitter, at(3599)),
        "a failed attempt is not retried inside the floor"
    );
    assert!(is_due(
        &[],
        &attempts,
        DESCRIPTIVE_TTL,
        &NoJitter,
        at(60 * 60)
    ));
}

#[test]
fn a_later_success_clears_an_earlier_failure() {
    let attempted = at(0);
    let fetched = at(120);
    let attempts = vec![attempt(
        attempted,
        Some("provider answered 404"),
        Some(fetched),
    )];

    assert_eq!(
        freshness(Some(fetched), attempts.first(), DESCRIPTIVE_TTL, at(180)),
        Freshness::Fresh
    );
    assert_eq!(
        absence(&[], &attempts, DESCRIPTIVE_TTL, at(180)),
        Some(AbsenceReason::ProviderReportedNone)
    );
}

#[test]
fn a_retry_after_a_failure_that_fails_again_stays_failed() {
    let attempts = vec![
        attempt(at(0), Some("first failure"), None),
        attempt(at(2 * 60 * 60), Some("second failure"), None),
    ];

    assert_eq!(
        freshness(None, attempts.last(), DESCRIPTIVE_TTL, at(2 * 60 * 60 + 60)),
        Freshness::Failed("second failure".to_owned())
    );
    assert!(
        !is_due(
            &[],
            &attempts,
            DESCRIPTIVE_TTL,
            &NoJitter,
            at(2 * 60 * 60 + 60)
        ),
        "the floor follows the latest attempt"
    );
}

#[test]
fn jitter_scales_the_ttl_within_the_configured_percent() {
    assert_eq!(JITTER_PERCENT, 20);

    let ttl = Duration::from_secs(1000);
    assert_eq!(jittered(ttl, &NoJitter), ttl);
    assert_eq!(
        jittered(ttl, &FixedJitter::new(1.0)),
        Duration::from_secs(1200)
    );
    assert_eq!(
        jittered(ttl, &FixedJitter::new(-1.0)),
        Duration::from_secs(800)
    );
    assert_eq!(
        jittered(ttl, &FixedJitter::new(0.5)),
        Duration::from_secs(1100)
    );
}

#[test]
fn the_negative_ttl_is_jittered_for_a_successful_empty() {
    let reported = at(0);
    let attempts = vec![attempt(reported, None, Some(reported))];
    let lowered = FixedJitter::new(-1.0);
    let raised = FixedJitter::new(1.0);
    let negative = 7 * 24 * 60 * 60;
    let shortest = negative * 80 / 100;
    let longest = negative * 120 / 100;

    assert!(
        !is_due(&[], &attempts, DESCRIPTIVE_TTL, &lowered, at(shortest - 1)),
        "negative jitter shortens the successful-empty window"
    );
    assert!(is_due(
        &[],
        &attempts,
        DESCRIPTIVE_TTL,
        &lowered,
        at(shortest)
    ));
    assert!(
        !is_due(&[], &attempts, DESCRIPTIVE_TTL, &raised, at(longest - 1)),
        "positive jitter extends the successful-empty window"
    );
    assert!(is_due(
        &[],
        &attempts,
        DESCRIPTIVE_TTL,
        &raised,
        at(longest)
    ));
}

#[test]
fn a_fact_fetched_after_the_failure_floor_instant_is_still_fresh() {
    let fetched = at(30 * 60);
    let attempts = [attempt(at(0), Some("earlier failure"), Some(fetched))];

    assert_eq!(
        freshness(
            Some(fetched),
            attempts.first(),
            DESCRIPTIVE_TTL,
            at(31 * 60)
        ),
        Freshness::Fresh
    );
}
