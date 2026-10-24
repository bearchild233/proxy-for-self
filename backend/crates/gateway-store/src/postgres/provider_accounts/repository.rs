//! Pg 账号 repository：Core/Admin 端口实现与 admin 事务。

use super::*;

fn quota_protection_storage_json(
    protection: gateway_core::account::AccountQuotaProtection,
) -> sqlx::types::Json<serde_json::Value> {
    let legacy = protection.used_percent_limit();
    // 统一阈值继续落旧格式，避免 A/B 重叠期间旧页面保存使排空中的旧进程无法读行。
    let value = if protection.short_term_used_percent_limit() == legacy
        && protection.weekly_used_percent_limit() == legacy
    {
        serde_json::json!({
            "enabled": protection.enabled(),
            "usedPercentLimit": legacy,
        })
    } else {
        serde_json::json!({
            "enabled": protection.enabled(),
            "usedPercentLimit": legacy,
            "shortTermUsedPercentLimit": protection.short_term_used_percent_limit(),
            "weeklyUsedPercentLimit": protection.weekly_used_percent_limit(),
        })
    };
    sqlx::types::Json(value)
}

#[async_trait]
pub trait ProviderAccountRepository: Send + Sync {
    async fn load_provider_account(&self, id: &str) -> StoreResult<Option<ProviderAccountRecord>>;
    async fn list_provider_accounts(
        &self,
        provider_kind: Option<&str>,
        include_disabled: bool,
    ) -> StoreResult<Vec<ProviderAccountSummary>>;
    async fn insert_provider_account(&self, account: NewProviderAccount) -> StoreResult<()>;
    async fn update_provider_account(&self, account: UpdateProviderAccount) -> StoreResult<bool>;
    async fn compare_and_swap_credentials(
        &self,
        update: ProviderCredentialUpdate,
    ) -> StoreResult<Revision>;
    async fn apply_provider_account_state(
        &self,
        update: ProviderAccountStateUpdate,
    ) -> StoreResult<bool>;
    async fn set_provider_account_enabled(&self, id: &str, enabled: bool) -> StoreResult<bool>;
    async fn compare_and_swap_provider_quota(
        &self,
        account_id: &str,
        expected_revision: Revision,
        quota: JsonObject,
        observed_at: DateTime<Utc>,
        state: QuotaState,
        plan_type: Option<&str>,
    ) -> StoreResult<bool>;
    async fn touch_provider_quota_observation(
        &self,
        account_id: &str,
        expected_revision: Revision,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<bool>;
    async fn apply_provider_quota_access(
        &self,
        account_id: &str,
        expected_revision: Revision,
        state: QuotaState,
    ) -> StoreResult<bool>;
    async fn delete_provider_account(&self, id: &str) -> StoreResult<bool>;
}

#[async_trait]
pub trait ProviderAccountAdminRepository: Send + Sync {
    async fn export_provider_accounts(
        &self,
        scope: ProviderAccountAdminScope,
        account_ids: Vec<String>,
    ) -> StoreResult<Vec<ProviderAccountRecord>>;

    async fn import_provider_accounts(
        &self,
        command: ImportProviderAccounts,
    ) -> StoreResult<ProviderAccountAdminImport>;

    async fn rotate_provider_account(
        &self,
        command: RotateProviderAccount,
    ) -> StoreResult<ProviderAccountAdminRotation>;

    async fn batch_update_provider_accounts_admin(
        &self,
        command: BatchUpdateProviderAccountsAdmin,
    ) -> StoreResult<Revision>;

    async fn recover_provider_account_admin(
        &self,
        command: RecoverProviderAccount,
    ) -> StoreResult<Revision>;

    async fn delete_provider_accounts_admin(
        &self,
        command: DeleteProviderAccounts,
    ) -> StoreResult<Revision>;
}

#[derive(Clone)]
pub struct PgProviderAccountRepository {
    pub(crate) pool: PgPool,
}

impl PgProviderAccountRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl gateway_core::provider_ports::ProviderGroupMovePort for PgProviderAccountRepository {
    fn move_account_group<'a>(
        &'a self,
        request: &'a gateway_core::provider_ports::AccountGroupMove,
    ) -> futures::future::BoxFuture<
        'a,
        Result<
            gateway_core::provider_ports::AccountGroupMoveOutcome,
            gateway_core::provider_ports::ProviderStoreError,
        >,
    > {
        use gateway_core::provider_ports::{ProviderStoreError, ProviderStoreErrorKind};
        Box::pin(async move {
            move_account_group_in_pool(&self.pool, request)
                .await
                .map_err(|error| match error {
                    StoreError::NotFound { .. } | StoreError::InvalidData { .. } => {
                        ProviderStoreError::new(
                            ProviderStoreErrorKind::InvalidData,
                            "move account group",
                        )
                    }
                    _ => ProviderStoreError::new(
                        ProviderStoreErrorKind::Unavailable,
                        "move account group",
                    ),
                })
        })
    }
}

#[async_trait]
impl ProviderAccountRepository for PgProviderAccountRepository {
    async fn load_provider_account(&self, id: &str) -> StoreResult<Option<ProviderAccountRecord>> {
        require_nonempty(ENTITY, "id", id)?;
        let row = sqlx::query(ACCOUNT_SELECT)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("load provider account"))?;
        row.map(account_record_from_row).transpose()
    }

    async fn list_provider_accounts(
        &self,
        provider_kind: Option<&str>,
        include_disabled: bool,
    ) -> StoreResult<Vec<ProviderAccountSummary>> {
        let rows = sqlx::query(
            "select location_country, location_region, location_city, location_timezone, outbound_proxy_url, id, provider_kind, name, notes, email, upstream_user_id,
                    upstream_account_id, plan_type, authentication_kind, credential_revision, has_refresh_token,
                    access_token_expires_at, next_refresh_at, enabled, concurrency_limit, weight, lifecycle_json, quota_protection_json, model_access_json, credential_state,
                    credential_observed_at, quota_access_state, quota_evidence,
                    quota_access_observed_at, quota_reset_at,
                    quota_observed_at, last_error_reason, last_error_message, created_at, updated_at
             from provider_accounts
             left join (select id as location_proxy_id, location_country, location_region, location_city, location_timezone from outbound_proxies) proxy_location
               on outbound_proxy_id = location_proxy_id
             where ($1::text is null or provider_kind = $1) and ($2 or enabled)
             order by provider_kind, name, id",
        )
        .bind(provider_kind)
        .bind(include_disabled)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("list provider accounts"))?;
        rows.into_iter().map(account_summary_from_row).collect()
    }

    async fn insert_provider_account(&self, account: NewProviderAccount) -> StoreResult<()> {
        account.validate()?;
        let credential_state = account.credential_state;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account insert"))?;
        let proxy_id = match account.outbound_proxy.as_ref() {
            Some(proxy) => Some(
                super::super::proxies::ensure_proxy_for_url(&mut transaction, proxy, None)
                    .await?
                    .0,
            ),
            None => None,
        };
        sqlx::query(
            "insert into provider_accounts (
               outbound_proxy_url, outbound_proxy_id, id, provider_kind, name, email, upstream_user_id,
               upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
               has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
               concurrency_limit, weight, model_access_json, credential_state, provider_quota_json,
               credential_observed_at, quota_access_observed_at, quota_observed_at, created_at, updated_at
             ) values (
               $18, $19, $1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10, $11, $12, $13,
               $14, $15, coalesce($20, '{\"mode\":\"all\",\"models\":[]}'::jsonb), $16, null, $17, null, null, now(), greatest(now(), $17)
             )",
        )
        .bind(&account.id)
        .bind(account.provider_kind)
        .bind(account.name)
        .bind(account.email)
        .bind(account.upstream_user_id)
        .bind(account.upstream_account_id)
        .bind(account.plan_type)
        .bind(account.authentication_kind)
        .bind(crate::vault::seal_json(&account.id, &account.provider_credentials_json.as_value())?)
        .bind(account.has_refresh_token)
        .bind(account.access_token_expires_at)
        .bind(account.next_refresh_at)
        .bind(account.enabled)
        .bind(account.concurrency_limit.map(|limit| i64::from(limit.get())))
        .bind(i16::try_from(account.weight.get()).map_err(|_| invalid("invalid weight"))?)
        .bind(credential_state.as_str())
        .bind(account.credential_observed_at)
        .bind(account.outbound_proxy.as_ref().map(|proxy| proxy.expose_url()))
        .bind(proxy_id)
        .bind(account.model_access.as_ref().map(sqlx::types::Json))
        .execute(&mut *transaction)
        .await
        .map_err(|_| postgres_unavailable("insert provider account"))?;
        transaction
            .commit()
            .await
            .map_err(|_| postgres_unavailable("commit provider account insert"))?;
        Ok(())
    }

    async fn update_provider_account(&self, account: UpdateProviderAccount) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", &account.id)?;
        require_nonempty(ENTITY, "name", &account.name)?;
        let result = sqlx::query(
            "update provider_accounts
             set name = case when name_customized then name else $2 end, email = $3, plan_type = $4, updated_at = greatest(now(), updated_at)
             where id = $1",
        )
        .bind(account.id)
        .bind(account.name)
        .bind(account.email)
        .bind(account.plan_type)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("update provider account"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn compare_and_swap_credentials(
        &self,
        update: ProviderCredentialUpdate,
    ) -> StoreResult<Revision> {
        require_nonempty(ENTITY, "account_id", &update.account_id)?;
        validate_object_size(
            "provider_credentials_json",
            &update.provider_credentials_json,
            CREDENTIALS_MAX_BYTES,
        )?;
        if !update.has_refresh_token && update.next_refresh_at.is_some() {
            return Err(invalid("next_refresh_at requires a refresh token"));
        }
        let next = sqlx::query_scalar::<_, i64>(
            "update provider_accounts
             set provider_credentials_json = $3,
                 credential_revision = credential_revision + 1,
                 has_refresh_token = $4,
                 access_token_expires_at = $5,
                 next_refresh_at = $6,
                 updated_at = greatest(now(), updated_at)
             where id = $1 and credential_revision = $2
             returning credential_revision",
        )
        .bind(&update.account_id)
        .bind(to_i64(update.expected_revision.get())?)
        .bind(crate::vault::seal_json(
            &update.account_id,
            &update.provider_credentials_json.as_value(),
        )?)
        .bind(update.has_refresh_token)
        .bind(update.access_token_expires_at)
        .bind(update.next_refresh_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("compare and swap provider credentials"))?
        .ok_or(StoreError::Conflict {
            entity: ENTITY,
            id: update.account_id,
            kind: ConflictKind::StaleRevision,
        })?;
        Revision::new(to_u64(next)?)
    }

    async fn apply_provider_account_state(
        &self,
        update: ProviderAccountStateUpdate,
    ) -> StoreResult<bool> {
        update.validate()?;
        let result = sqlx::query(
            "update provider_accounts
             set credential_state = case
                     when enabled then $3
                     else credential_state
                 end,
                 credential_observed_at = case
                     when enabled then $4
                     else credential_observed_at
                 end,
                 last_error_reason = case
                     when enabled then $5
                     else last_error_reason
                 end,
                 last_error_message = case
                     when enabled then $6
                     else last_error_message
                 end,
                 updated_at = case when enabled then greatest(now(), updated_at, $4) else updated_at end
             where id = $1 and credential_revision = $2
               and (credential_observed_at is null or credential_observed_at <= $4)",
        )
        .bind(update.account_id)
        .bind(to_i64(update.expected_revision.get())?)
        .bind(update.credential_state.as_str())
        .bind(update.credential_observed_at)
        .bind(update.error_reason.map(AccountErrorReason::as_str))
        .bind(update.message.as_deref())
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("apply provider account state"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn set_provider_account_enabled(&self, id: &str, enabled: bool) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", id)?;
        let result = sqlx::query(
            "update provider_accounts set enabled = $2 and coalesce(lifecycle_json->>'archived', 'false') <> 'true', updated_at = greatest(now(), updated_at) where id = $1",
        )
        .bind(id)
        .bind(enabled)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("set provider account enabled state"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn compare_and_swap_provider_quota(
        &self,
        account_id: &str,
        expected_revision: Revision,
        quota: JsonObject,
        observed_at: DateTime<Utc>,
        state: QuotaState,
        plan_type: Option<&str>,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "account_id", account_id)?;
        validate_object_size("provider_quota_json", &quota, QUOTA_MAX_BYTES)?;
        let access_observed_at = state.observed_at().map(DateTime::<Utc>::from);
        let result = sqlx::query(
            "update provider_accounts
             set provider_quota_json = $3, quota_observed_at = $4,
                 plan_type = coalesce($9, plan_type),
                 quota_access_state = case
                   when $7::timestamptz is not null
                     and (quota_access_observed_at is null or quota_access_observed_at <= $7)
                   then $5 else quota_access_state end,
                 quota_evidence = case
                   when $7::timestamptz is not null
                     and (quota_access_observed_at is null or quota_access_observed_at <= $7)
                   then $6 else quota_evidence end,
                 quota_access_observed_at = case
                   when $7::timestamptz is not null
                     and (quota_access_observed_at is null or quota_access_observed_at <= $7)
                   then $7 else quota_access_observed_at end,
                 quota_reset_at = case
                   when $7::timestamptz is not null
                     and (quota_access_observed_at is null or quota_access_observed_at <= $7)
                   then $8 else quota_reset_at end,
                 updated_at = greatest(now(), updated_at, $4, $7)
             where id = $1 and credential_revision = $2
               and (quota_observed_at is null or quota_observed_at <= $4)",
        )
        .bind(account_id)
        .bind(to_i64(expected_revision.get())?)
        .bind(quota.as_value())
        .bind(observed_at)
        .bind(state.access().as_str())
        .bind(state.evidence().map(QuotaEvidence::as_str))
        .bind(access_observed_at)
        .bind(state.reset_at().map(DateTime::<Utc>::from))
        .bind(plan_type)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("compare and swap provider quota"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn apply_provider_quota_access(
        &self,
        account_id: &str,
        expected_revision: Revision,
        state: QuotaState,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "account_id", account_id)?;
        let observed_at = state
            .observed_at()
            .map(DateTime::<Utc>::from)
            .ok_or_else(|| invalid("quota access change requires observed_at"))?;
        let result = sqlx::query(
            "update provider_accounts
             set quota_access_observed_at = $3, quota_access_state = $4,
                 quota_evidence = $5, quota_reset_at = $6,
                 updated_at = greatest(now(), updated_at, $3)
             where id = $1 and credential_revision = $2
               and (quota_access_observed_at is null or quota_access_observed_at <= $3)",
        )
        .bind(account_id)
        .bind(to_i64(expected_revision.get())?)
        .bind(observed_at)
        .bind(state.access().as_str())
        .bind(state.evidence().map(QuotaEvidence::as_str))
        .bind(state.reset_at().map(DateTime::<Utc>::from))
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("apply provider quota access"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn touch_provider_quota_observation(
        &self,
        account_id: &str,
        expected_revision: Revision,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "account_id", account_id)?;
        let result = sqlx::query(
            "update provider_accounts
             set quota_observed_at = $3, updated_at = greatest(now(), updated_at, $3)
             where id = $1 and credential_revision = $2
               and provider_quota_json is not null
               and (quota_observed_at is null or quota_observed_at <= $3)",
        )
        .bind(account_id)
        .bind(to_i64(expected_revision.get())?)
        .bind(observed_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("touch provider quota observation"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn delete_provider_account(&self, id: &str) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", id)?;
        let result = sqlx::query("delete from provider_accounts where id = $1 and not enabled")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("delete disabled provider account"))?;
        Ok(result.rows_affected() == 1)
    }
}

#[async_trait]
impl ProviderAccountAdminRepository for PgProviderAccountRepository {
    async fn export_provider_accounts(
        &self,
        scope: ProviderAccountAdminScope,
        account_ids: Vec<String>,
    ) -> StoreResult<Vec<ProviderAccountRecord>> {
        scope.validate()?;
        validate_admin_account_ids(&account_ids)?;
        let rows = sqlx::query(ACCOUNT_SELECT_BY_IDS)
            .bind(&account_ids)
            .bind(&scope.provider_kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("export provider accounts"))?;
        let records = rows
            .into_iter()
            .map(account_record_from_row)
            .collect::<StoreResult<Vec<_>>>()?;
        if records.len() != account_ids.len() {
            return Err(invalid(
                "one or more exported accounts are missing or outside the Provider scope",
            ));
        }
        let by_id = records
            .into_iter()
            .map(|record| (record.summary.id.clone(), record))
            .collect::<std::collections::HashMap<_, _>>();
        account_ids
            .into_iter()
            .map(|id| {
                by_id.get(&id).cloned().ok_or_else(|| {
                    invalid("one or more exported accounts are missing after loading")
                })
            })
            .collect()
    }

    async fn import_provider_accounts(
        &self,
        command: ImportProviderAccounts,
    ) -> StoreResult<ProviderAccountAdminImport> {
        command.validate()?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin import"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            if let Some(binding) = &command.outbound_proxy {
                let (_, current) = super::super::proxies::resolve_proxy_selection(
                    &mut transaction,
                    &gateway_admin::model::proxies::AccountProxySelection::Saved(
                        binding.id.clone(),
                    ),
                )
                .await?;
                if current.as_ref() != Some(&binding.proxy) {
                    return Err(StoreError::Conflict {
                        entity: "outbound proxy",
                        id: binding.id.clone(),
                        kind: ConflictKind::StaleRevision,
                    });
                }
            }
            let mut account_ids = Vec::with_capacity(command.accounts.len());
            for account in &command.accounts {
                account_ids
                    .push(upsert_provider_account_in_transaction(&mut transaction, account).await?);
            }
            if let Some(settings) = &command.settings {
                let unique_ids = account_ids
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                update_provider_accounts_scheduling_in_transaction(
                    &mut transaction,
                    &unique_ids,
                    Some(settings.enabled),
                    Some(settings.concurrency_limit),
                    Some(settings.weight),
                    settings.model_access.as_ref(),
                    None,
                )
                .await?;
                replace_account_group_assignments_in_transaction(
                    &mut transaction,
                    &unique_ids,
                    &settings.group_ids,
                )
                .await?;
                if let Some(notes) = settings.notes.as_deref() {
                    update_provider_account_notes_in_transaction(
                        &mut transaction,
                        &unique_ids,
                        notes,
                    )
                    .await?;
                }
            }
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(ProviderAccountAdminImport {
                config_revision: revision,
                account_ids,
            })
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin import").await
    }

    async fn rotate_provider_account(
        &self,
        command: RotateProviderAccount,
    ) -> StoreResult<ProviderAccountAdminRotation> {
        command.scope.validate()?;
        require_nonempty(ENTITY, "account_id", &command.profile.id)?;
        require_nonempty(ENTITY, "name", &command.profile.name)?;
        if let Some(identity) = &command.replacement_identity {
            require_nonempty(ENTITY, "upstream_user_id", identity.upstream_user_id())?;
            if let Some(account_id) = identity.upstream_account_id() {
                require_nonempty(ENTITY, "upstream_account_id", account_id)?;
            }
        }
        if command.profile.id != command.credential.account_id {
            return Err(invalid("rotated profile and credential account IDs differ"));
        }
        validate_credential_update(&command.credential)?;
        if let Some(settings) = &command.settings {
            if settings.account_id != command.profile.id {
                return Err(invalid(
                    "rotated credential and account settings IDs differ",
                ));
            }
            validate_batch_update_group_ids(&settings.group_ids)?;
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin rotation"))?;
        let result = async {
            let config_revision = bump_config_revision_in_transaction(&mut transaction).await?;
            // 重新授权成功的归档账号先离开归档并启用，再轮换凭据：
            // 轮换按“已启用账号”重置凭据状态与错误信息，且凭据 CAS 失败时两步一起回滚。
            let reactivated = command.reactivate_archived
                && reactivate_archived_account_in_transaction(
                    &mut transaction,
                    &command.scope,
                    &command.profile.id,
                )
                .await?;
            let credential_revision = rotate_provider_account_in_transaction(
                &mut transaction,
                &command.scope,
                &command.profile,
                command.replacement_identity.as_ref(),
                &command.credential,
            )
            .await?;
            // 凭据 CAS、普通设置和审计共享事务，任何设置失败都回滚凭据更新。
            if let Some(settings) = &command.settings {
                let ids = std::slice::from_ref(&settings.account_id);
                update_provider_accounts_scheduling_in_transaction(
                    &mut transaction,
                    ids,
                    Some(settings.enabled),
                    Some(settings.concurrency_limit),
                    Some(settings.weight),
                    settings.model_access.as_ref(),
                    settings.outbound_proxy.as_ref(),
                )
                .await?;
                replace_account_group_assignments_in_transaction(
                    &mut transaction,
                    ids,
                    &settings.group_ids,
                )
                .await?;
                if let Some(protection) = settings.quota_protection {
                    sqlx::query(
                        "update provider_accounts set quota_protection_json=$2 where id=any($1)",
                    )
                    .bind(ids)
                    .bind(quota_protection_storage_json(protection))
                    .execute(&mut *transaction)
                    .await
                    .map_err(|_| postgres_unavailable("set quota protection"))?;
                }
                if let Some(notes) = settings.notes.as_deref() {
                    update_provider_account_notes_in_transaction(&mut transaction, ids, notes)
                        .await?;
                }
            }
            let mut audit = command.audit;
            if reactivated {
                // 审计需要能看出这次重新授权同时解除了归档并启用了账号。
                for field in ["enabled", "restore_archived"] {
                    if !audit.changed_fields.iter().any(|changed| changed == field) {
                        audit.changed_fields.push(field.to_owned());
                    }
                }
            }
            append_admin_audit_event_in_transaction(&mut transaction, audit, config_revision)
                .await?;
            Ok(ProviderAccountAdminRotation {
                config_revision,
                credential_revision,
            })
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin rotation").await
    }

    async fn batch_update_provider_accounts_admin(
        &self,
        command: BatchUpdateProviderAccountsAdmin,
    ) -> StoreResult<Revision> {
        validate_batch_update_account_ids(&command.account_ids)?;
        if let Some(name) = command.name.as_ref()
            && (command.account_ids.len() != 1
                || name.trim().is_empty()
                || name.chars().count() > 100
                || name.chars().any(char::is_control))
        {
            return Err(StoreError::InvalidData {
                entity: "provider account",
                message: "invalid account name".to_owned(),
            });
        }
        if let Some(group_ids) = &command.group_ids {
            validate_batch_update_group_ids(group_ids)?;
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin state change"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            update_provider_accounts_scheduling_in_transaction(
                &mut transaction,
                &command.account_ids,
                command.enabled,
                command.concurrency_limit,
                command.weight,
                command.model_access.as_ref(),
                command.outbound_proxy.as_ref(),
            )
            .await?;
            if let Some(protection) = command.quota_protection {
                sqlx::query("update provider_accounts set quota_protection_json=$2 where id=any($1)")
                    .bind(&command.account_ids).bind(quota_protection_storage_json(protection))
                    .execute(&mut *transaction).await
                    .map_err(|_| postgres_unavailable("set quota protection"))?;
            }
            if command.expiry_priority == Some(true) {
                let valid: i64 = sqlx::query_scalar("select count(*) from provider_accounts where id=any($1)
                    and provider_kind='openai' and authentication_kind='oauth' and lower(plan_type)='plus'")
                    .bind(&command.account_ids).fetch_one(&mut *transaction).await
                    .map_err(|_| postgres_unavailable("validate expiry priority"))?;
                if usize::try_from(valid).ok() != Some(command.account_ids.len()) {
                    return Err(StoreError::InvalidData { entity: "provider account", message: "expiry priority requires Plus accounts".to_owned() });
                }
            }
            if let Some(priority) = command.expiry_priority {
                sqlx::query("update provider_accounts set lifecycle_json=jsonb_set(lifecycle_json, '{expiryPriority}', to_jsonb($2::boolean)) where id=any($1)")
                    .bind(&command.account_ids).bind(priority).execute(&mut *transaction).await
                    .map_err(|_| postgres_unavailable("set expiry priority"))?;
            }
            // 保留上次归档时间：恢复后停用期间不被维护任务立即重新归档。
            if command.restore_archived {
                sqlx::query("update provider_accounts set enabled=false,
                    lifecycle_json=lifecycle_json || '{\"archived\":false,\"archiveReason\":null}'::jsonb
                    where id=any($1) and lifecycle_json->>'archived'='true'")
                    .bind(&command.account_ids).execute(&mut *transaction).await
                    .map_err(|_| postgres_unavailable("restore archived accounts"))?;
            }
            sqlx::query("update provider_accounts set enabled=false where id=any($1) and lifecycle_json->>'archived'='true'")
                .bind(&command.account_ids).execute(&mut *transaction).await
                .map_err(|_| postgres_unavailable("preserve archived state"))?;
            if let Some(group_ids) = &command.group_ids {
                replace_account_group_assignments_in_transaction(
                    &mut transaction,
                    &command.account_ids,
                    group_ids,
                )
                .await?;
            }
            if let Some(name) = command.name.as_deref() {
                sqlx::query("update provider_accounts set name=$2, name_customized=true, updated_at=greatest(now(), updated_at) where id=$1")
                    .bind(&command.account_ids[0])
                    .bind(name.trim())
                    .execute(&mut *transaction)
                    .await
                    .map_err(|_| postgres_unavailable("rename provider account"))?;
            }
            if let Some(notes) = command.notes.as_deref() {
                update_provider_account_notes_in_transaction(
                    &mut transaction,
                    &command.account_ids,
                    notes,
                )
                .await?;
            }
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(revision)
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin state change").await
    }

    async fn recover_provider_account_admin(
        &self,
        command: RecoverProviderAccount,
    ) -> StoreResult<Revision> {
        validate_admin_account_ids(std::slice::from_ref(&command.account_id))?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin recovery"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            let recovered = sqlx::query_scalar::<_, String>(
                "update provider_accounts
                 set enabled = coalesce(lifecycle_json->>'archived', 'false') <> 'true',
                     credential_state = case when credential_state = 'unknown' then 'unknown' else 'ready' end,
                     credential_observed_at = now(),
                     access_token_expires_at = case
                         when access_token_expires_at <= now() then null
                         else access_token_expires_at
                     end,
                     provider_quota_json = null,
                     quota_observed_at = null,
                     quota_access_state = 'allowed',
                     quota_evidence = null,
                     quota_access_observed_at = now(),
                     quota_reset_at = null,
                     last_error_reason = null,
                     last_error_message = null,
                     updated_at = greatest(now(), updated_at)
                 where id = $1
                 returning id",
            )
            .bind(&command.account_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| postgres_unavailable("recover provider account state"))?
            .ok_or_else(|| StoreError::NotFound {
                entity: ENTITY,
                id: command.account_id.clone(),
            })?;
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok((revision, recovered))
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin recovery")
            .await
            .map(|(revision, _)| revision)
    }

    async fn delete_provider_accounts_admin(
        &self,
        command: DeleteProviderAccounts,
    ) -> StoreResult<Revision> {
        command.scope.validate()?;
        validate_admin_account_ids(&command.account_ids)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin deletion"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            delete_provider_accounts_in_transaction(
                &mut transaction,
                &command.scope,
                &command.account_ids,
            )
            .await?;
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(revision)
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin deletion").await
    }
}

async fn update_provider_account_notes_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account_ids: &[String],
    notes: &str,
) -> StoreResult<()> {
    sqlx::query("update provider_accounts set notes = nullif($2, '') where id = any($1::text[])")
        .bind(account_ids)
        .bind(notes.trim())
        .execute(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("update provider account notes"))?;
    Ok(())
}

async fn replace_account_group_assignments_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account_ids: &[String],
    group_ids: &[AccountGroupId],
) -> StoreResult<()> {
    let group_ids = group_ids
        .iter()
        .map(|group_id| group_id.as_str().to_owned())
        .collect::<Vec<_>>();
    if !group_ids.is_empty() {
        let known_group_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from account_groups where id = any($1::text[])",
        )
        .bind(&group_ids)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("validate account group assignment"))?;
        if usize::try_from(known_group_count).ok() != Some(group_ids.len()) {
            return Err(StoreError::NotFound {
                entity: "account group",
                id: "one or more group IDs".to_owned(),
            });
        }
    }
    sqlx::query("delete from account_group_accounts where provider_account_id = any($1::text[])")
        .bind(account_ids)
        .execute(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("clear account group assignments"))?;
    if group_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "insert into account_group_accounts
         (account_group_id, provider_account_id, created_at)
         select group_id, account_id, now()
         from unnest($1::text[]) group_id
         cross join unnest($2::text[]) account_id",
    )
    .bind(group_ids)
    .bind(account_ids)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("assign accounts to groups"))?;
    Ok(())
}

pub(crate) async fn upsert_provider_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account: &NewProviderAccount,
) -> StoreResult<String> {
    account.validate()?;
    let credential_state = account.credential_state;
    let proxy_id = match account.outbound_proxy.as_ref() {
        Some(proxy) => Some(
            super::super::proxies::ensure_proxy_for_url(transaction, proxy, None)
                .await?
                .0,
        ),
        None => None,
    };
    let imported_id = sqlx::query_scalar::<_, String>(
        "insert into provider_accounts (
           outbound_proxy_url, outbound_proxy_id, id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
           concurrency_limit, weight, model_access_json, credential_state, provider_quota_json,
           credential_observed_at, quota_access_observed_at, quota_observed_at, created_at, updated_at
         ) values (
           $18, $19, $1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10, $11, $12, $13,
           $14, $15, coalesce($20, '{\"mode\":\"all\",\"models\":[]}'::jsonb), $16, null, $17, null, null, now(), greatest(now(), $17)
         )
         on conflict (
           provider_kind,
           upstream_user_id,
           (coalesce(upstream_account_id, ''))
         ) do update set
           model_access_json = coalesce($20, provider_accounts.model_access_json),
           name = case when provider_accounts.name_customized then provider_accounts.name else excluded.name end,
           email = excluded.email,
           plan_type = excluded.plan_type,
           authentication_kind = excluded.authentication_kind,
           provider_credentials_json = excluded.provider_credentials_json,
           outbound_proxy_url = coalesce(excluded.outbound_proxy_url, provider_accounts.outbound_proxy_url),
           outbound_proxy_id = coalesce(excluded.outbound_proxy_id, provider_accounts.outbound_proxy_id),
           credential_revision = provider_accounts.credential_revision + 1,
           has_refresh_token = excluded.has_refresh_token,
           access_token_expires_at = excluded.access_token_expires_at,
           next_refresh_at = excluded.next_refresh_at,
           enabled = excluded.enabled and coalesce(provider_accounts.lifecycle_json->>'archived', 'false') <> 'true',
           credential_state = excluded.credential_state,
           provider_quota_json = null,
           quota_access_state = 'unknown',
           quota_evidence = null,
           quota_access_observed_at = null,
           quota_reset_at = null,
           credential_observed_at = excluded.credential_observed_at,
           quota_observed_at = null,
           last_error_reason = null,
           last_error_message = null,
           updated_at = greatest(now(), provider_accounts.updated_at, excluded.credential_observed_at)
         returning id",
    )
    .bind(&account.id)
    .bind(&account.provider_kind)
    .bind(&account.name)
    .bind(&account.email)
    .bind(&account.upstream_user_id)
    .bind(&account.upstream_account_id)
    .bind(&account.plan_type)
    .bind(&account.authentication_kind)
    .bind(crate::vault::seal_json(&account.id, &account.provider_credentials_json.as_value())?)
    .bind(account.has_refresh_token)
    .bind(account.access_token_expires_at)
    .bind(account.next_refresh_at)
    .bind(account.enabled)
    .bind(account.concurrency_limit.map(|limit| i64::from(limit.get())))
    .bind(i16::try_from(account.weight.get()).map_err(|_| invalid("invalid weight"))?)
    .bind(credential_state.as_str())
    .bind(account.credential_observed_at)
    .bind(account.outbound_proxy.as_ref().map(|proxy| proxy.expose_url()))
    .bind(proxy_id)
    .bind(account.model_access.as_ref().map(sqlx::types::Json))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
        {
            StoreError::Conflict {
                entity: ENTITY,
                id: account.id.clone(),
                kind: ConflictKind::InvalidTransition,
            }
        } else {
            postgres_unavailable("upsert provider account in admin transaction")
        }
    })?
    .ok_or_else(|| StoreError::Conflict {
        entity: ENTITY,
        id: account.id.clone(),
        kind: ConflictKind::InvalidTransition,
    })?;
    Ok(imported_id)
}

/// 归档账号离开归档并启用；保留 `archivedAt`，与“恢复到主列表”一致，
/// 维护任务只会按新的凭据与订阅事实重新判断。返回是否确实发生了状态变化。
async fn reactivate_archived_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    account_id: &str,
) -> StoreResult<bool> {
    let updated = sqlx::query_scalar::<_, String>(
        "update provider_accounts
         set enabled = true,
             lifecycle_json = lifecycle_json || '{\"archived\":false,\"archiveReason\":null}'::jsonb,
             updated_at = greatest(now(), updated_at)
         where id = $1 and provider_kind = $2 and lifecycle_json->>'archived' = 'true'
         returning id",
    )
    .bind(account_id)
    .bind(&scope.provider_kind)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("reactivate archived provider account"))?;
    Ok(updated.is_some())
}

pub(crate) async fn rotate_provider_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    profile: &UpdateProviderAccount,
    replacement_identity: Option<&ProviderAccountIdentity>,
    update: &ProviderCredentialUpdate,
) -> StoreResult<Revision> {
    let replace_identity = replacement_identity.is_some();
    let upstream_user_id = replacement_identity.map(ProviderAccountIdentity::upstream_user_id);
    let upstream_account_id =
        replacement_identity.and_then(ProviderAccountIdentity::upstream_account_id);
    // 事务时间可能早于应用写入的额度观测，保留既有时间下界，避免轮换破坏时间约束。
    let next = sqlx::query_scalar::<_, i64>(
        "update provider_accounts
         set name = case when $14 or name_customized then name else $4 end,
             email = case when $14 then email else $5 end,
             plan_type = case when $14 then plan_type else $6 end,
             provider_credentials_json = $7,
             credential_revision = credential_revision + 1,
             has_refresh_token = $8,
             access_token_expires_at = $9,
             next_refresh_at = $10,
             upstream_user_id = case when $11::boolean then $12::text else upstream_user_id end,
             upstream_account_id = case when $11::boolean then $13::text else upstream_account_id end,
             lifecycle_json = case when $11::boolean and upstream_account_id is distinct from $13::text
                 then lifecycle_json - 'subscriptionExpiresAt' - 'subscriptionObservedAt' else lifecycle_json end,
             credential_state = case
                 when not enabled then credential_state
                 when $11::boolean or credential_state <> 'unknown' then 'ready'
                 else 'unknown'
             end,
             credential_observed_at = case
                 when not enabled then credential_observed_at
                 else now()
             end,
             last_error_reason = case when enabled then null else last_error_reason end,
             last_error_message = case when enabled then null else last_error_message end,
             updated_at = greatest(now(), updated_at)
         where id = $1 and provider_kind = $2
           and credential_revision = $3
         returning credential_revision",
    )
    .bind(&update.account_id)
    .bind(&scope.provider_kind)
    .bind(to_i64(update.expected_revision.get())?)
    .bind(&profile.name)
    .bind(&profile.email)
    .bind(&profile.plan_type)
    .bind(crate::vault::seal_json(&update.account_id, &update.provider_credentials_json.as_value())?)
    .bind(update.has_refresh_token)
    .bind(update.access_token_expires_at)
    .bind(update.next_refresh_at)
    .bind(replace_identity)
    .bind(upstream_user_id)
    .bind(upstream_account_id)
    .bind(update.preserve_profile)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
        {
            StoreError::Conflict {
                entity: ENTITY,
                id: update.account_id.clone(),
                kind: ConflictKind::InvalidTransition,
            }
        } else {
            postgres_unavailable("rotate provider account in admin transaction")
        }
    })?
    .ok_or_else(|| StoreError::Conflict {
        entity: ENTITY,
        id: update.account_id.clone(),
        kind: ConflictKind::StaleRevision,
    })?;
    Revision::new(to_u64(next)?)
}

pub(crate) async fn update_provider_accounts_scheduling_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account_ids: &[String],
    enabled: Option<bool>,
    concurrency_limit: Option<Option<AccountConcurrencyLimit>>,
    weight: Option<AccountWeight>,
    model_access: Option<&gateway_core::account::AccountModelAccess>,
    outbound_proxy: Option<&gateway_admin::model::proxies::AccountProxySelection>,
) -> StoreResult<()> {
    let (proxy_id, proxy) = match outbound_proxy {
        Some(selection) => {
            super::super::proxies::resolve_proxy_selection(transaction, selection).await?
        }
        None => (None, None),
    };
    let updated = sqlx::query_scalar::<_, String>(
        "update provider_accounts
         set enabled = coalesce($2, enabled) and coalesce(lifecycle_json->>'archived', 'false') <> 'true', concurrency_limit = case when $9 then $3 else concurrency_limit end, weight = coalesce($4, weight), updated_at = greatest(now(), updated_at),
             outbound_proxy_url = case when $5 then $6 else outbound_proxy_url end,
             outbound_proxy_id = case when $5 then $7 else outbound_proxy_id end,
             model_access_json = coalesce($8, model_access_json)
         where id = any($1::text[])
         returning id",
    )
    .bind(account_ids)
    .bind(enabled)
    .bind(concurrency_limit.flatten().map(|limit| i64::from(limit.get())))
    .bind(weight.map(|weight| i16::try_from(weight.get())).transpose().map_err(|_| invalid("invalid weight"))?)
    .bind(outbound_proxy.is_some())
    .bind(proxy.as_ref().map(gateway_core::account::OutboundProxy::expose_url))
    .bind(proxy_id)
    .bind(model_access.map(sqlx::types::Json))
    .bind(concurrency_limit.is_some())
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("set provider accounts state in admin transaction"))?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let expected = account_ids.iter().cloned().collect::<BTreeSet<_>>();
    if updated == expected {
        Ok(())
    } else {
        Err(StoreError::NotFound {
            entity: ENTITY,
            id: "one or more provider account IDs".to_owned(),
        })
    }
}

pub(crate) async fn delete_provider_accounts_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    account_ids: &[String],
) -> StoreResult<()> {
    let deleted = sqlx::query_scalar::<_, String>(
        "delete from provider_accounts
         where id = any($1::text[]) and provider_kind = $2
         returning id",
    )
    .bind(account_ids)
    .bind(&scope.provider_kind)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("delete provider account in admin transaction"))?;
    let deleted = deleted.into_iter().collect::<BTreeSet<_>>();
    let expected = account_ids.iter().cloned().collect::<BTreeSet<_>>();
    if deleted == expected {
        Ok(())
    } else {
        Err(invalid(
            "all deleted accounts must exist and match Provider scope",
        ))
    }
}

pub(crate) fn validate_credential_update(update: &ProviderCredentialUpdate) -> StoreResult<()> {
    require_nonempty(ENTITY, "account_id", &update.account_id)?;
    validate_object_size(
        "provider_credentials_json",
        &update.provider_credentials_json,
        CREDENTIALS_MAX_BYTES,
    )?;
    if !update.has_refresh_token && update.next_refresh_at.is_some() {
        return Err(invalid("next_refresh_at requires a refresh token"));
    }
    Ok(())
}

pub(crate) fn validate_admin_account_ids(account_ids: &[String]) -> StoreResult<()> {
    if account_ids.is_empty() || account_ids.len() > MAX_ADMIN_IMPORT_BATCH {
        return Err(invalid(
            "admin account selection must contain between 1 and 200 IDs",
        ));
    }
    let mut unique = BTreeSet::new();
    for account_id in account_ids {
        require_nonempty(ENTITY, "account_id", account_id)?;
        if !unique.insert(account_id.as_str()) {
            return Err(invalid("admin account selection contains duplicate IDs"));
        }
    }
    Ok(())
}

fn validate_batch_update_account_ids(account_ids: &[String]) -> StoreResult<()> {
    const MAX_BATCH_UPDATE_ACCOUNTS: usize = 1000;
    if account_ids.is_empty() || account_ids.len() > MAX_BATCH_UPDATE_ACCOUNTS {
        return Err(invalid(
            "account batch update must contain between 1 and 1000 IDs",
        ));
    }
    let mut unique = BTreeSet::new();
    for account_id in account_ids {
        require_nonempty(ENTITY, "account_id", account_id)?;
        if !unique.insert(account_id.as_str()) {
            return Err(invalid("account batch update contains duplicate IDs"));
        }
    }
    Ok(())
}

pub(super) fn validate_batch_update_group_ids(group_ids: &[AccountGroupId]) -> StoreResult<()> {
    const MAX_BATCH_UPDATE_GROUPS: usize = 1000;
    if group_ids.len() > MAX_BATCH_UPDATE_GROUPS {
        return Err(invalid("account batch update contains too many group IDs"));
    }
    let mut unique = BTreeSet::new();
    if group_ids
        .iter()
        .any(|group_id| !unique.insert(group_id.as_str()))
    {
        return Err(invalid("account batch update contains duplicate group IDs"));
    }
    Ok(())
}

/// 网关池或插件调整账号分组：只增删指定成员关系，同一事务提升配置版本并写审计。
pub(crate) async fn move_account_group_in_pool(
    pool: &PgPool,
    request: &gateway_core::provider_ports::AccountGroupMove,
) -> StoreResult<gateway_core::provider_ports::AccountGroupMoveOutcome> {
    use gateway_core::provider_ports::AccountGroupMoveOutcome;
    let account_id = request.account.as_str();
    validate_admin_account_ids(std::slice::from_ref(&account_id.to_owned()))?;
    let add =
        AccountGroupId::new(request.add_group.clone()).map_err(|_| StoreError::InvalidData {
            entity: "account group",
            message: "invalid group ID".to_owned(),
        })?;
    let remove = request
        .remove_group
        .as_ref()
        .map(|group| AccountGroupId::new(group.clone()))
        .transpose()
        .map_err(|_| StoreError::InvalidData {
            entity: "account group",
            message: "invalid group ID".to_owned(),
        })?;
    if remove.as_ref() == Some(&add) {
        return Err(StoreError::InvalidData {
            entity: "account group",
            message: "source and target groups must differ".to_owned(),
        });
    }
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| postgres_unavailable("begin account group move"))?;
    let result = async {
        let exists = sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from provider_accounts where id = $1)",
        )
        .bind(account_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| postgres_unavailable("check account for group move"))?;
        if !exists {
            return Err(StoreError::NotFound {
                entity: ENTITY,
                id: account_id.to_owned(),
            });
        }
        let known = sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from account_groups where id = $1)",
        )
        .bind(add.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| postgres_unavailable("check target account group"))?;
        if !known {
            return Err(StoreError::NotFound {
                entity: "account group",
                id: add.as_str().to_owned(),
            });
        }
        // 先锁住账号的成员行再判断，避免与管理端同时改分组时丢更新。
        let current = sqlx::query_scalar::<_, String>(
            "select account_group_id from account_group_accounts
             where provider_account_id = $1 for update",
        )
        .bind(account_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| postgres_unavailable("read account group membership"))?;
        let has_add = current.iter().any(|group| group == add.as_str());
        let has_remove = remove
            .as_ref()
            .is_some_and(|group| current.iter().any(|item| item == group.as_str()));
        if has_add && !has_remove {
            return Ok(AccountGroupMoveOutcome::Unchanged);
        }
        let revision = bump_config_revision_in_transaction(&mut transaction).await?;
        if let Some(group) = remove.as_ref().filter(|_| has_remove) {
            sqlx::query(
                "delete from account_group_accounts
                 where provider_account_id = $1 and account_group_id = $2",
            )
            .bind(account_id)
            .bind(group.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(|_| postgres_unavailable("leave account group"))?;
        }
        if !has_add {
            sqlx::query(
                "insert into account_group_accounts
                 (account_group_id, provider_account_id, created_at)
                 values ($1, $2, now()) on conflict do nothing",
            )
            .bind(add.as_str())
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| postgres_unavailable("join account group"))?;
        }
        let audit = AdminAuditEvent {
            id: format!("audit_{}", uuid::Uuid::now_v7().simple()),
            actor_kind: crate::postgres::AdminAuditActorKind::System,
            actor_admin_user_id: None,
            // 插件身份与原因由内核能力端口构造；其他来源沿用原审计操作者。
            actor_ref: if request.reason.starts_with("plugin:") {
                request.reason.clone()
            } else {
                "gateway_pool".to_owned()
            },
            admin_request_id: None,
            action: "regroup".to_owned(),
            entity_kind: "provider_account".to_owned(),
            entity_ref: account_id.to_owned(),
            config_revision: None,
            changed_fields: vec!["groups".to_owned()],
            created_at: Utc::now(),
        };
        append_admin_audit_event_in_transaction(&mut transaction, audit, revision).await?;
        Ok(AccountGroupMoveOutcome::Moved)
    }
    .await;
    finish_admin_transaction(transaction, result, "account group move").await
}

pub(crate) async fn finish_admin_transaction<T>(
    transaction: Transaction<'_, Postgres>,
    result: StoreResult<T>,
    operation: &'static str,
) -> StoreResult<T> {
    match result {
        Ok(value) => {
            transaction
                .commit()
                .await
                .map_err(|_| postgres_unavailable(operation))?;
            Ok(value)
        }
        Err(error) => {
            transaction
                .rollback()
                .await
                .map_err(|_| postgres_unavailable(operation))?;
            Err(error)
        }
    }
}
