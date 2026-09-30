use chrono::{Duration, Utc};
use gateway_core::account::{AccountLifecycle, AccountWeight};

#[test]
fn expiry_priority_requires_opt_in_plus_and_fresh_future_expiry() {
    let now = Utc::now();
    let base = AccountWeight::new(23).unwrap();
    let mut facts = AccountLifecycle {
        expiry_priority: true,
        subscription_observed_at: Some(now),
        subscription_expires_at: Some(now + Duration::days(5)),
        ..Default::default()
    };
    assert_eq!(facts.effective_weight(base, true, now).get(), 100);
    assert_eq!(facts.effective_weight(base, false, now), base);
    for expiry in [
        None,
        Some(now),
        Some(now - Duration::seconds(1)),
        Some(now + Duration::days(6)),
    ] {
        facts.subscription_expires_at = expiry;
        assert_eq!(facts.effective_weight(base, true, now), base);
    }
    facts.subscription_expires_at = Some(now + Duration::days(1));
    facts.subscription_observed_at = Some(now - Duration::hours(25));
    assert_eq!(facts.effective_weight(base, true, now), base);
    facts.subscription_observed_at = Some(now);
    facts.expiry_priority = false;
    assert_eq!(facts.effective_weight(base, true, now), base);
    facts.expiry_priority = true;
    facts.archived = true;
    assert_eq!(facts.effective_weight(base, true, now), base);
    assert!(
        !super::account("acct_archived")
            .with_lifecycle(facts)
            .enabled()
    );
    assert_eq!(AccountWeight::DEFAULT.get(), 50);
}
