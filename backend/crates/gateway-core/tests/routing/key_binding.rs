use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gateway_core::account::ProviderAccountId;
use gateway_core::routing::{
    AccountRoutingScopeKind, ClientRoutingScope, FrozenAccountScope, ProviderKind, RuntimeAccount,
    RuntimeAccountDirectory,
};

fn account(value: &str) -> ProviderAccountId {
    ProviderAccountId::new(value).expect("valid account")
}

fn directory() -> Arc<RuntimeAccountDirectory> {
    let provider = ProviderKind::new("openai").expect("provider");
    Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([
        (
            account("acct_a"),
            RuntimeAccount::new(provider.clone(), BTreeSet::new()),
        ),
        (
            account("acct_b"),
            RuntimeAccount::new(provider, BTreeSet::new()),
        ),
    ])))
}

fn fixed(id: &str) -> FrozenAccountScope {
    let directory = directory();
    let scope = ClientRoutingScope::fixed_account(account(id), &directory);
    FrozenAccountScope::new(directory, scope)
}

#[test]
fn fixed_binding_never_selects_another_account_of_the_same_provider() {
    let scope = fixed("acct_a");
    assert!(scope.allows(&account("acct_a")));
    assert!(!scope.allows(&account("acct_b")));
    assert!(scope.allows_model(&account("acct_a"), "gpt-test"));
    assert!(!scope.allows_model(&account("acct_b"), "gpt-test"));
}

#[test]
fn removed_or_unknown_bound_account_has_no_eligible_provider() {
    let scope = fixed("acct_missing");
    assert!(!scope.allows(&account("acct_a")));
    assert!(!scope.allows(&account("acct_missing")));
    assert!(scope.provider_kinds().is_empty());
}

#[test]
fn explicit_denied_scope_does_not_expand_to_all_accounts() {
    let scope = FrozenAccountScope::new(directory(), ClientRoutingScope::denied());
    assert!(!scope.allows(&account("acct_a")));
    assert!(!scope.allows(&account("acct_b")));
    assert!(scope.provider_kinds().is_empty());
    assert_eq!(
        scope.routing_snapshot().kind(),
        AccountRoutingScopeKind::Denied
    );
}

#[test]
fn plugin_toggle_is_frozen_per_key_not_shared_by_account() {
    let native_key = fixed("acct_b");
    let excel_key = native_key.clone().with_excel_bridge_enabled(true);
    assert!(!native_key.excel_bridge_enabled());
    assert!(excel_key.excel_bridge_enabled());
    assert!(native_key.allows(&account("acct_b")));
    assert!(excel_key.allows(&account("acct_b")));
    assert!(!excel_key.allows(&account("acct_a")));
}

#[test]
fn disabling_one_key_plugin_leaves_existing_snapshot_unchanged() {
    let in_flight = fixed("acct_b").with_excel_bridge_enabled(true);
    let next_request = in_flight.clone().with_excel_bridge_enabled(false);
    assert!(in_flight.excel_bridge_enabled());
    assert!(!next_request.excel_bridge_enabled());
}

#[test]
fn audit_scope_records_the_fixed_account_instead_of_all_accounts() {
    let scope = fixed("acct_a");
    let snapshot = scope.routing_snapshot();
    assert_eq!(snapshot.kind(), AccountRoutingScopeKind::Account);
    assert_eq!(snapshot.account_id(), Some(&account("acct_a")));
    assert!(snapshot.groups_snapshot().is_empty());
}
