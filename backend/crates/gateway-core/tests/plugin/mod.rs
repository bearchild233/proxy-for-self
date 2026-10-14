mod capabilities;

#[test]
fn upstream_attempt_budget_is_shared_and_never_exceeds_four() {
    let budget = gateway_core::plugin::PluginAttemptBudget::default();
    let cloned = budget.clone();
    assert_eq!(budget.next_attempt(), 1);
    assert_eq!(budget.claim(), Some(1));
    assert_eq!(cloned.claim(), Some(2));
    assert_eq!(budget.claim(), Some(3));
    assert_eq!(cloned.claim(), Some(4));
    assert_eq!(budget.next_attempt(), 5);
    assert!(!budget.available());
    assert_eq!(cloned.claim(), None);
}
