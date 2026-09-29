//! 请求认证时冻结的账号范围与目录，不依赖路由选择器。

use super::ProviderAccountId;
use crate::identity::ProviderKind;
use crate::validation::{IdentifierError, RoutingError};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

static EMPTY_PROVIDER_KINDS: BTreeSet<ProviderKind> = BTreeSet::new();

/// `account_groups.id` 的核心值对象。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountGroupId(String);

impl AccountGroupId {
    /// 校验并创建账号分组 ID。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        let Some(suffix) = value.strip_prefix("grp_") else {
            return Err(IdentifierError::MissingPrefix { expected: "grp_" });
        };
        if suffix.len() != 32
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(IdentifierError::InvalidFormat);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountGroupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 快照中一个账号的 Provider 与分组归属。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAccount {
    provider_kind: ProviderKind,
    group_ids: Arc<BTreeSet<AccountGroupId>>,
    model_access: super::AccountModelAccess,
}

impl RuntimeAccount {
    #[must_use]
    pub fn new(provider_kind: ProviderKind, group_ids: BTreeSet<AccountGroupId>) -> Self {
        Self {
            provider_kind,
            group_ids: Arc::new(group_ids),
            model_access: super::AccountModelAccess::all(),
        }
    }

    #[must_use]
    pub fn with_model_access(mut self, model_access: super::AccountModelAccess) -> Self {
        self.model_access = model_access;
        self
    }

    #[must_use]
    pub const fn model_access(&self) -> &super::AccountModelAccess {
        &self.model_access
    }

    #[must_use]
    pub const fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    #[must_use]
    pub fn group_ids(&self) -> &BTreeSet<AccountGroupId> {
        &self.group_ids
    }
}

/// 全快照共享的账号、Provider 与分组反向索引。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeAccountDirectory {
    accounts: BTreeMap<ProviderAccountId, RuntimeAccount>,
    providers_with_accounts: BTreeSet<ProviderKind>,
    providers_by_group: BTreeMap<AccountGroupId, BTreeSet<ProviderKind>>,
}

impl RuntimeAccountDirectory {
    #[must_use]
    pub fn new(accounts: BTreeMap<ProviderAccountId, RuntimeAccount>) -> Self {
        let mut providers_with_accounts = BTreeSet::new();
        let mut providers_by_group = BTreeMap::<AccountGroupId, BTreeSet<ProviderKind>>::new();
        for account in accounts.values() {
            providers_with_accounts.insert(account.provider_kind.clone());
            for group_id in account.group_ids.iter() {
                providers_by_group
                    .entry(group_id.clone())
                    .or_default()
                    .insert(account.provider_kind.clone());
            }
        }
        Self {
            accounts,
            providers_with_accounts,
            providers_by_group,
        }
    }

    #[must_use]
    pub fn account(&self, account_id: &ProviderAccountId) -> Option<&RuntimeAccount> {
        self.accounts.get(account_id)
    }

    #[must_use]
    pub fn providers_with_accounts(&self) -> &BTreeSet<ProviderKind> {
        &self.providers_with_accounts
    }

    #[must_use]
    pub fn providers_for_groups<'a>(
        &self,
        group_ids: impl IntoIterator<Item = &'a AccountGroupId>,
    ) -> BTreeSet<ProviderKind> {
        group_ids
            .into_iter()
            .filter_map(|group_id| self.providers_by_group.get(group_id))
            .flatten()
            .cloned()
            .collect()
    }
}

/// 历史请求保存的账号范围种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountRoutingScopeKind {
    All,
    Groups,
    Account,
    Accounts,
    Denied,
}

impl AccountRoutingScopeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Groups => "groups",
            Self::Account => "account",
            Self::Accounts => "accounts",
            Self::Denied => "denied",
        }
    }
}

/// 请求开始时冻结的分组 ID 与名称。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingGroupSnapshot {
    id: AccountGroupId,
    name: String,
}

impl RoutingGroupSnapshot {
    #[must_use]
    pub fn new(id: AccountGroupId, name: String) -> Self {
        Self { id, name }
    }

    #[must_use]
    pub const fn id(&self) -> &AccountGroupId {
        &self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// 请求历史所需的完整、稳定账号范围快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRoutingSnapshot {
    kind: AccountRoutingScopeKind,
    groups: Arc<[RoutingGroupSnapshot]>,
    account_id: Option<ProviderAccountId>,
    account_ids: Arc<[ProviderAccountId]>,
}

impl AccountRoutingSnapshot {
    #[must_use]
    pub fn all() -> Self {
        Self {
            kind: AccountRoutingScopeKind::All,
            groups: Arc::from([]),
            account_id: None,
            account_ids: Arc::from([]),
        }
    }

    #[must_use]
    pub fn groups(groups: Vec<RoutingGroupSnapshot>) -> Self {
        Self {
            kind: AccountRoutingScopeKind::Groups,
            groups: Arc::from(groups),
            account_id: None,
            account_ids: Arc::from([]),
        }
    }

    #[must_use]
    pub fn account(account_id: ProviderAccountId) -> Self {
        Self {
            kind: AccountRoutingScopeKind::Account,
            groups: Arc::from([]),
            account_id: Some(account_id),
            account_ids: Arc::from([]),
        }
    }

    #[must_use]
    pub fn denied() -> Self {
        Self {
            kind: AccountRoutingScopeKind::Denied,
            groups: Arc::from([]),
            account_id: None,
            account_ids: Arc::from([]),
        }
    }

    #[must_use]
    pub fn accounts(account_ids: Vec<ProviderAccountId>) -> Self {
        Self {
            kind: AccountRoutingScopeKind::Accounts,
            groups: Arc::from([]),
            account_id: None,
            account_ids: Arc::from(account_ids),
        }
    }

    #[must_use]
    pub fn account_ids(&self) -> &[ProviderAccountId] {
        &self.account_ids
    }

    #[must_use]
    pub const fn account_id(&self) -> Option<&ProviderAccountId> {
        self.account_id.as_ref()
    }

    #[must_use]
    pub const fn kind(&self) -> AccountRoutingScopeKind {
        self.kind
    }

    #[must_use]
    pub fn groups_snapshot(&self) -> &[RoutingGroupSnapshot] {
        &self.groups
    }
}

/// Key 持久 binding 编译出的账号权限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRoutingScope {
    AllAccounts,
    Denied,
    Accounts {
        account_ids: Arc<BTreeSet<ProviderAccountId>>,
        provider_kinds: Arc<BTreeSet<ProviderKind>>,
    },
    FixedAccount {
        account_id: ProviderAccountId,
        provider_kinds: Arc<BTreeSet<ProviderKind>>,
    },
    Restricted {
        bound_groups: Arc<[RoutingGroupSnapshot]>,
        enabled_group_ids: Arc<BTreeSet<AccountGroupId>>,
        provider_kinds: Arc<BTreeSet<ProviderKind>>,
    },
}

impl ClientRoutingScope {
    /// 无绑定或被撤销的绑定必须拒绝，不能回落为全账号权限。
    #[must_use]
    pub const fn denied() -> Self {
        Self::Denied
    }

    #[must_use]
    pub fn fixed_account(
        account_id: ProviderAccountId,
        directory: &RuntimeAccountDirectory,
    ) -> Self {
        let provider_kinds = directory
            .account(&account_id)
            .map(|account| BTreeSet::from([account.provider_kind().clone()]))
            .unwrap_or_default();
        Self::FixedAccount {
            account_id,
            provider_kinds: Arc::new(provider_kinds),
        }
    }

    #[must_use]
    pub fn accounts(
        account_ids: BTreeSet<ProviderAccountId>,
        directory: &RuntimeAccountDirectory,
    ) -> Self {
        let provider_kinds = account_ids
            .iter()
            .filter_map(|id| directory.account(id))
            .map(|account| account.provider_kind().clone())
            .collect();
        Self::Accounts {
            account_ids: Arc::new(account_ids),
            provider_kinds: Arc::new(provider_kinds),
        }
    }

    #[must_use]
    pub fn all_accounts() -> Self {
        Self::AllAccounts
    }

    pub fn restricted(
        bound_groups: Vec<RoutingGroupSnapshot>,
        enabled_group_ids: BTreeSet<AccountGroupId>,
        provider_kinds: BTreeSet<ProviderKind>,
    ) -> Result<Self, RoutingError> {
        if bound_groups.is_empty() {
            return Err(RoutingError::InvalidAccountScope);
        }
        Ok(Self::Restricted {
            bound_groups: Arc::from(bound_groups),
            enabled_group_ids: Arc::new(enabled_group_ids),
            provider_kinds: Arc::new(provider_kinds),
        })
    }
}

/// 一次认证随 RuntimeSnapshot 冻结的账号目录与 Key 权限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenAccountScope {
    eligible_accounts: Option<Arc<BTreeSet<ProviderAccountId>>>,
    eligible_providers: Option<Arc<BTreeSet<ProviderKind>>>,
    rotation_strategy: Option<super::RotationStrategy>,
    disable_fast: bool,
    excel_bridge_enabled: bool,
    binding_revision: u64,
    request_profiles: BTreeMap<ProviderKind, super::OpaqueProviderData>,
    directory: Arc<RuntimeAccountDirectory>,
    client_scope: ClientRoutingScope,
}

impl FrozenAccountScope {
    #[must_use]
    pub fn with_eligible_accounts(mut self, ids: BTreeSet<ProviderAccountId>) -> Self {
        let providers = ids
            .iter()
            .filter(|id| self.allows(id))
            .filter_map(|id| self.directory.account(id))
            .map(|account| account.provider_kind().clone())
            .collect();
        self.eligible_accounts = Some(Arc::new(ids));
        self.eligible_providers = Some(Arc::new(providers));
        self
    }

    #[must_use]
    pub const fn with_rotation_strategy(
        mut self,
        strategy: Option<super::RotationStrategy>,
    ) -> Self {
        self.rotation_strategy = strategy;
        self
    }

    #[must_use]
    pub const fn rotation_strategy(&self) -> Option<super::RotationStrategy> {
        self.rotation_strategy
    }

    #[must_use]
    pub const fn with_binding_revision(mut self, revision: u64) -> Self {
        self.binding_revision = revision;
        self
    }

    #[must_use]
    pub const fn binding_revision(&self) -> u64 {
        self.binding_revision
    }

    /// 插件选择属于当前 Key 的快照，不修改账号或同账号的其他 Key。
    #[must_use]
    pub const fn with_excel_bridge_enabled(mut self, enabled: bool) -> Self {
        self.excel_bridge_enabled = enabled;
        self
    }

    #[must_use]
    pub const fn excel_bridge_enabled(&self) -> bool {
        self.excel_bridge_enabled
    }

    /// 与 Key 授权范围一同冻结；具体字段只由对应 Provider 解释。
    #[must_use]
    pub fn with_request_profiles(
        mut self,
        profiles: BTreeMap<ProviderKind, super::OpaqueProviderData>,
    ) -> Self {
        self.request_profiles = profiles;
        self
    }

    #[must_use]
    pub fn request_profile(&self, provider: &ProviderKind) -> Option<&super::OpaqueProviderData> {
        self.request_profiles.get(provider)
    }

    /// Key 绑定分组的冻结 Fast 限制，与账号成员资格无关。
    #[must_use]
    pub const fn with_disable_fast(mut self, disable_fast: bool) -> Self {
        self.disable_fast = disable_fast;
        self
    }

    #[must_use]
    pub const fn disable_fast(&self) -> bool {
        self.disable_fast
    }

    #[must_use]
    pub const fn new(
        directory: Arc<RuntimeAccountDirectory>,
        client_scope: ClientRoutingScope,
    ) -> Self {
        Self {
            eligible_accounts: None,
            eligible_providers: None,
            rotation_strategy: None,
            disable_fast: false,
            excel_bridge_enabled: false,
            binding_revision: 0,
            request_profiles: BTreeMap::new(),
            directory,
            client_scope,
        }
    }

    #[must_use]
    pub fn allows(&self, account_id: &ProviderAccountId) -> bool {
        if self
            .eligible_accounts
            .as_ref()
            .is_some_and(|ids| !ids.contains(account_id))
        {
            return false;
        }
        let Some(account) = self.directory.account(account_id) else {
            return false;
        };
        match &self.client_scope {
            ClientRoutingScope::AllAccounts => true,
            ClientRoutingScope::Denied => false,
            ClientRoutingScope::Accounts { account_ids, .. } => account_ids.contains(account_id),
            ClientRoutingScope::FixedAccount {
                account_id: bound_account,
                provider_kinds,
            } => bound_account == account_id && provider_kinds.contains(account.provider_kind()),
            ClientRoutingScope::Restricted {
                enabled_group_ids, ..
            } => account
                .group_ids()
                .iter()
                .any(|group_id| enabled_group_ids.contains(group_id)),
        }
    }

    #[must_use]
    pub fn allows_model(&self, account_id: &ProviderAccountId, upstream_model: &str) -> bool {
        self.allows(account_id)
            && self
                .directory
                .account(account_id)
                .is_some_and(|account| account.model_access.allows(upstream_model))
    }

    /// 目录按整个授权账号池过滤，不能只看用于获取元数据的账号政策。
    #[must_use]
    pub fn allows_provider_model(&self, provider: &ProviderKind, upstream_model: &str) -> bool {
        self.directory.accounts.iter().any(|(id, account)| {
            account.provider_kind() == provider && self.allows_model(id, upstream_model)
        })
    }

    #[must_use]
    pub fn provider_kinds(&self) -> &BTreeSet<ProviderKind> {
        if let Some(providers) = &self.eligible_providers {
            return providers;
        }
        match &self.client_scope {
            ClientRoutingScope::AllAccounts => self.directory.providers_with_accounts(),
            ClientRoutingScope::Denied => &EMPTY_PROVIDER_KINDS,
            ClientRoutingScope::Accounts { provider_kinds, .. } => provider_kinds,
            ClientRoutingScope::FixedAccount { provider_kinds, .. } => provider_kinds,
            ClientRoutingScope::Restricted { provider_kinds, .. } => provider_kinds,
        }
    }

    #[must_use]
    pub fn routing_snapshot(&self) -> AccountRoutingSnapshot {
        match &self.client_scope {
            ClientRoutingScope::AllAccounts => AccountRoutingSnapshot::all(),
            ClientRoutingScope::Denied => AccountRoutingSnapshot::denied(),
            ClientRoutingScope::Accounts { account_ids, .. } => {
                AccountRoutingSnapshot::accounts(account_ids.iter().cloned().collect())
            }
            ClientRoutingScope::FixedAccount { account_id, .. } => {
                AccountRoutingSnapshot::account(account_id.clone())
            }
            ClientRoutingScope::Restricted { bound_groups, .. } => {
                AccountRoutingSnapshot::groups(bound_groups.to_vec())
            }
        }
    }

    #[must_use]
    pub const fn directory(&self) -> &Arc<RuntimeAccountDirectory> {
        &self.directory
    }
}

/// 每把 Key 的账号来源和调度覆盖，不承载凭据或会话内容。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyRoutingOptions {
    pub mode: String,
    #[serde(default)]
    pub account_ids: Vec<String>,
    #[serde(default)]
    pub group_ids: Vec<String>,
    #[serde(default)]
    pub rotation_strategy: Option<String>,
}

impl KeyRoutingOptions {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let unique = |values: &[String]| {
            values.len() <= 200 && values.iter().collect::<BTreeSet<_>>().len() == values.len()
        };
        unique(&self.account_ids)
            && unique(&self.group_ids)
            && self
                .account_ids
                .iter()
                .all(|id| ProviderAccountId::new(id.clone()).is_ok())
            && self
                .group_ids
                .iter()
                .all(|id| AccountGroupId::new(id.clone()).is_ok())
            && self
                .rotation_strategy
                .as_deref()
                .is_none_or(|value| super::RotationStrategy::parse(value).is_some())
            && match self.mode.as_str() {
                "account" => self.account_ids.len() == 1 && self.group_ids.is_empty(),
                "accounts" => !self.account_ids.is_empty() && self.group_ids.is_empty(),
                "groups" => self.account_ids.is_empty() && !self.group_ids.is_empty(),
                _ => false,
            }
    }
}
