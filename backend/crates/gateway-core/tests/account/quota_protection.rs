use gateway_core::account::{
    AccountEligibilityPolicy, AccountQuotaProtection, AccountQuotaProtectionUsage,
    AccountQuotaSignals, AccountSelector, AccountStatus, CredentialState, QuotaAccessState,
    QuotaState, RotationStrategy,
};
use serde_json::json;

use super::{account, candidate, context};

#[test]
fn quota_protection_defaults_and_wire_validation_preserve_a_real_reserve() {
    let default = AccountQuotaProtection::default();
    assert!(!default.enabled());
    assert_eq!(default.used_percent_limit(), 90);
    for limit in [1, 90, 99] {
        let config = AccountQuotaProtection::new(true, limit).expect("valid reserve");
        let wire = json!({"enabled": true, "usedPercentLimit": limit});
        assert_eq!(
            serde_json::to_value(config).unwrap(),
            json!({
                "enabled": true, "usedPercentLimit": limit,
                "shortTermUsedPercentLimit": limit, "weeklyUsedPercentLimit": limit
            })
        );
        assert_eq!(
            serde_json::from_value::<AccountQuotaProtection>(wire).unwrap(),
            config
        );
    }
    for limit in [0, 100, 255] {
        assert!(AccountQuotaProtection::new(true, limit).is_none());
        assert!(
            serde_json::from_value::<AccountQuotaProtection>(
                json!({"enabled": true, "usedPercentLimit": limit})
            )
            .is_err()
        );
    }
    assert!(
        serde_json::from_value::<AccountQuotaProtection>(
            json!({"enabled": true, "usedPercentLimit": 89.5})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<AccountQuotaProtection>(
            json!({"enabled": true, "usedPercentLimit": 90, "typo": true})
        )
        .is_err()
    );
}

#[test]
fn independent_window_limits_round_trip_and_reject_partial_or_invalid_configuration() {
    let policy = AccountQuotaProtection::new(true, 80)
        .unwrap()
        .with_window_limits(95, 85)
        .unwrap();
    let wire = json!({"enabled":true,"usedPercentLimit":80,
        "shortTermUsedPercentLimit":95,"weeklyUsedPercentLimit":85});
    assert_eq!(serde_json::to_value(policy).unwrap(), wire);
    assert_eq!(
        serde_json::from_value::<AccountQuotaProtection>(wire.clone()).unwrap(),
        policy
    );
    for key in ["shortTermUsedPercentLimit", "weeklyUsedPercentLimit"] {
        for invalid in [json!(0), json!(100), json!(90.5), json!("90"), json!(null)] {
            let mut bad = wire.clone();
            bad[key] = invalid;
            assert!(serde_json::from_value::<AccountQuotaProtection>(bad).is_err());
        }
        let mut partial = wire.clone();
        partial.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<AccountQuotaProtection>(partial).is_err());
    }
    for (short, weekly) in [(1, 99), (99, 1)] {
        assert!(policy.with_window_limits(short, weekly).is_some());
    }
    assert!(policy.with_window_limits(0, 90).is_none());
    assert!(policy.with_window_limits(90, 100).is_none());
}

#[test]
fn each_window_uses_its_own_limit_without_the_legacy_limit_interfering() {
    let policy = AccountQuotaProtection::new(true, 80)
        .unwrap()
        .with_window_limits(95, 85)
        .unwrap();
    for (short, weekly, other, reached) in [
        (Some(94.999), Some(84.999), None, false),
        (Some(95.0), Some(20.0), None, true),
        (Some(20.0), Some(85.0), None, true),
        (Some(90.0), Some(82.0), None, false),
        (None, None, Some(80.0), true),
        (None, None, Some(79.999), false),
        (None, None, None, false),
    ] {
        let usage = AccountQuotaProtectionUsage::new(short, weekly, other);
        let signals = AccountQuotaSignals::new(None, Some(1234)).with_protection_usage(usage);
        assert_eq!(policy.reached_usage(signals.protection_usage()), reached);
        assert_eq!(signals.remaining_rank(), Some(1234));
        assert!(!AccountQuotaProtection::default().reached_usage(usage));
    }
    let invalid = AccountQuotaProtectionUsage::new(Some(f64::NAN), Some(f64::INFINITY), Some(-1.0));
    assert_eq!(invalid, AccountQuotaProtectionUsage::UNKNOWN);
}

#[test]
fn protection_threshold_is_inclusive_and_unknown_or_nonfinite_usage_does_not_block() {
    let enabled = AccountQuotaProtection::new(true, 90).unwrap();
    for usage in [
        None,
        Some(0.0),
        Some(89.999),
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(f64::NEG_INFINITY),
    ] {
        assert!(!enabled.reached(usage), "usage {usage:?}");
    }
    for usage in [90.0, 90.001, 100.0] {
        assert!(enabled.reached(Some(usage)), "usage {usage}");
        assert!(!AccountQuotaProtection::default().reached(Some(usage)));
    }
}

#[test]
fn protection_usage_signal_does_not_round_up_to_the_threshold_or_change_ranking() {
    let config = AccountQuotaProtection::new(true, 90).unwrap();
    for (usage, expected, reached) in [
        (89.999, 89.0, false),
        (90.0, 90.0, true),
        (99.9, 99.0, true),
    ] {
        let signals =
            AccountQuotaSignals::new(None, Some(1234)).with_protection_used_percent(Some(usage));
        assert_eq!(signals.remaining_rank(), Some(1234));
        assert_eq!(signals.protection_used_percent(), Some(expected));
        assert_eq!(config.reached(signals.protection_used_percent()), reached);
    }
    for usage in [None, Some(f64::NAN), Some(f64::INFINITY)] {
        assert_eq!(
            AccountQuotaSignals::new(None, None)
                .with_protection_used_percent(usage)
                .protection_used_percent(),
            None
        );
    }
}

#[test]
fn protection_blocks_routing_without_overwriting_provider_quota_and_preserves_status_priority() {
    let now = std::time::SystemTime::now();
    let protected = account("acct_protected")
        .with_account_facts(
            true,
            CredentialState::Ready,
            QuotaState::allowed(now),
            None,
            None,
        )
        .with_quota_protection(AccountQuotaProtection::new(true, 90).unwrap())
        .with_quota_protection_reached(true);
    assert_eq!(protected.quota().access(), QuotaAccessState::Allowed);
    let cooling_projection =
        protected.status_projection(now, Some((now + std::time::Duration::from_secs(60)).into()));
    assert_eq!(cooling_projection.status, AccountStatus::QuotaExhausted);
    assert_eq!(cooling_projection.cooldown, None);
    assert_eq!(
        protected.status_projection(now, None).status,
        AccountStatus::QuotaExhausted
    );
    assert_eq!(
        protected
            .clone()
            .with_quota_protection_reached(false)
            .status_projection(now, None)
            .status,
        AccountStatus::Normal
    );
    assert_eq!(
        protected
            .clone()
            .with_account_facts(
                false,
                CredentialState::Ready,
                QuotaState::allowed(now),
                None,
                None
            )
            .status_projection(now, None)
            .status,
        AccountStatus::Disabled
    );
    assert_eq!(
        protected
            .with_account_facts(
                true,
                CredentialState::Banned,
                QuotaState::allowed(now),
                None,
                None
            )
            .status_projection(now, None)
            .status,
        AccountStatus::Error
    );
}

#[test]
fn protection_precedes_preferred_account_but_diagnostics_keep_the_existing_bypass() {
    let mut protected = candidate("acct_protected", 0, Some(9999));
    protected.account = protected
        .account
        .with_quota_protection(AccountQuotaProtection::new(true, 90).unwrap())
        .with_quota_protection_reached(true);
    let available = candidate("acct_available", 0, Some(1000));
    for strategy in [
        RotationStrategy::Smart,
        RotationStrategy::Sticky,
        RotationStrategy::RoundRobin,
        RotationStrategy::QuotaResetPriority,
    ] {
        let mut selection = context(strategy);
        selection.preferred_account = Some(protected.account.id().clone());
        selection.preferred_account_overrides_weight = true;
        let candidates = [protected.clone(), available.clone()];
        assert_eq!(
            AccountSelector
                .select(&candidates, &selection)
                .unwrap()
                .candidate()
                .account
                .id(),
            available.account.id()
        );
        assert!(
            AccountSelector
                .select(&candidates[..1], &selection)
                .is_none()
        );
        assert!(
            AccountSelector
                .wait_candidates(&candidates[..1], &selection)
                .is_empty()
        );
        selection.eligibility = AccountEligibilityPolicy::BypassForDiagnostic;
        assert!(
            AccountSelector
                .select(&candidates[..1], &selection)
                .is_some()
        );
    }
}
