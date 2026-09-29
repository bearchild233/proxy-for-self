//! 固定账号绑定和 Key 级插件设置的事务边界。

use gateway_admin::model::client_keys::{
    ClientKeyBinding, ClientKeyBindingMutation, ClientKeyBindings, SetClientKeyBinding,
};
use gateway_admin::model::{MutationContext, Revision};
use gateway_admin::ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult};
use gateway_core::account::ProviderAccountId;
use gateway_core::policy::ClientApiKeyId;
use sqlx::{PgPool, Postgres, Transaction};

pub(super) async fn write_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    id: &ClientApiKeyId,
    account_id: Option<&ProviderAccountId>,
    routing: Option<&gateway_core::account::scope::KeyRoutingOptions>,
    excel_bridge_enabled: bool,
    binding_revision: crate::Revision,
) -> AdminStoreResult<()> {
    let key =
        sqlx::query_scalar::<_, String>("select id from client_api_keys where id = $1 for update")
            .bind(id.as_str())
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|_| error(AdminStoreErrorKind::Unavailable, "lock bound key"))?;
    if key.is_none() {
        return Err(error(AdminStoreErrorKind::NotFound, "key does not exist"));
    }
    if let Some(routing) = routing {
        if !routing.is_valid()
            || (routing.mode == "account"
                && account_id.map(ProviderAccountId::as_str)
                    != routing.account_ids.first().map(String::as_str))
            || (routing.mode != "account" && account_id.is_some())
        {
            return Err(error(AdminStoreErrorKind::Invalid, "invalid routing scope"));
        }
    } else if account_id.is_none() {
        return Err(error(AdminStoreErrorKind::Invalid, "missing account scope"));
    }
    let ids: Vec<String> = routing.map_or_else(
        || account_id.iter().map(|id| id.as_str().to_owned()).collect(),
        |r| r.account_ids.clone(),
    );
    for id in &ids {
        let account = sqlx::query_as::<_, (String, String)>("select provider_kind, authentication_kind from provider_accounts where id=$1 for key share")
            .bind(id).fetch_optional(&mut **transaction).await.map_err(|_| error(AdminStoreErrorKind::Unavailable, "lock account"))?
            .ok_or_else(|| error(AdminStoreErrorKind::NotFound, "account does not exist"))?;
        if account.0 != "openai" || (excel_bridge_enabled && account.1 != "oauth") {
            return Err(error(
                AdminStoreErrorKind::Invalid,
                "unsupported account or Excel authentication kind",
            ));
        }
    }
    if let Some(routing) = routing {
        for id in &routing.group_ids {
            let exists = sqlx::query_scalar::<_, String>(
                "select id from account_groups where id=$1 for key share",
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|_| error(AdminStoreErrorKind::Unavailable, "lock group"))?;
            if exists.is_none() {
                return Err(error(AdminStoreErrorKind::NotFound, "group does not exist"));
            }
        }
    }
    sqlx::query(
        "insert into client_key_bindings (client_api_key_id, provider_account_id, excel_bridge_enabled, binding_revision, routing_options)
         values ($1, $2, $3, $4, $5)
         on conflict (client_api_key_id) do update set
           provider_account_id = excluded.provider_account_id,
           excel_bridge_enabled = excluded.excel_bridge_enabled,
           binding_revision = excluded.binding_revision, routing_options = excluded.routing_options, updated_at = now()",
    ).bind(id.as_str()).bind(account_id.map(ProviderAccountId::as_str)).bind(excel_bridge_enabled)
        .bind(i64::try_from(binding_revision.get()).map_err(|_| error(AdminStoreErrorKind::Invalid, "binding revision overflow"))?)
        .bind(routing.map(sqlx::types::Json))
        .execute(&mut **transaction).await
        .map_err(|_| error(AdminStoreErrorKind::Unavailable, "write key binding"))?;
    // 固定账号 Key 继承统一客户端基准，不能另行覆盖 Native 身份。
    sqlx::query(
        "update client_api_keys set provider_request_profiles_json = '{}'::jsonb where id = $1",
    )
    .bind(id.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(|_| {
        error(
            AdminStoreErrorKind::Unavailable,
            "clear key identity overrides",
        )
    })?;
    Ok(())
}

const RESOURCE: &str = "client key binding";

fn error(kind: AdminStoreErrorKind, message: &'static str) -> AdminStoreError {
    AdminStoreError::new(kind, RESOURCE, message)
}

pub(super) async fn load(
    pool: &PgPool,
    ids: &[ClientApiKeyId],
) -> AdminStoreResult<ClientKeyBindings> {
    if ids.is_empty() || ids.len() > 200 {
        return Err(error(
            AdminStoreErrorKind::Invalid,
            "invalid binding query size",
        ));
    }
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| error(AdminStoreErrorKind::Unavailable, "begin binding read"))?;
    sqlx::query("set transaction isolation level repeatable read, read only")
        .execute(&mut *transaction)
        .await
        .map_err(|_| error(AdminStoreErrorKind::Unavailable, "configure binding read"))?;
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| error(AdminStoreErrorKind::Unavailable, "read binding revision"))?;
    let keys: Vec<&str> = ids.iter().map(ClientApiKeyId::as_str).collect();
    let rows = sqlx::query_as::<_, (String, Option<String>, bool, Option<sqlx::types::Json<gateway_core::account::scope::KeyRoutingOptions>>)>(
        "select k.id, b.provider_account_id, coalesce(b.excel_bridge_enabled, false), b.routing_options
         from client_api_keys k
         left join client_key_bindings b on b.client_api_key_id = k.id
         where k.id = any($1) order by k.id",
    )
    .bind(keys)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|_| error(AdminStoreErrorKind::Unavailable, "read key bindings"))?;
    let items =
        rows.into_iter()
            .map(|(id, account_id, excel_bridge_enabled, routing)| {
                Ok(ClientKeyBinding {
                    routing: routing.map(|value| value.0),
                    id: ClientApiKeyId::new(id).map_err(|_| {
                        error(AdminStoreErrorKind::Unavailable, "invalid stored key id")
                    })?,
                    account_id: account_id.map(ProviderAccountId::new).transpose().map_err(
                        |_| {
                            error(
                                AdminStoreErrorKind::Unavailable,
                                "invalid stored account id",
                            )
                        },
                    )?,
                    excel_bridge_enabled,
                })
            })
            .collect::<AdminStoreResult<Vec<_>>>()?;
    transaction
        .commit()
        .await
        .map_err(|_| error(AdminStoreErrorKind::Unavailable, "finish binding read"))?;
    let config_revision = u64::try_from(revision)
        .ok()
        .and_then(|n| Revision::new(n).ok())
        .ok_or_else(|| error(AdminStoreErrorKind::Unavailable, "invalid binding revision"))?;
    Ok(ClientKeyBindings {
        config_revision,
        items,
    })
}

pub(super) async fn set(
    pool: &PgPool,
    command: SetClientKeyBinding,
    context: &MutationContext,
) -> AdminStoreResult<ClientKeyBindingMutation> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| error(AdminStoreErrorKind::Unavailable, "begin binding mutation"))?;
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id = 1 for update")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| error(AdminStoreErrorKind::Unavailable, "lock binding revision"))?;
    if u64::try_from(revision).ok() != Some(command.expected_revision.get()) {
        return Err(error(
            AdminStoreErrorKind::StaleRevision,
            "binding revision changed",
        ));
    }
    let revision = super::bump_config_revision_in_transaction(&mut transaction)
        .await
        .map_err(|e| crate::admin_store_error(RESOURCE, e))?;
    write_in_transaction(
        &mut transaction,
        &command.id,
        command.account_id.as_ref(),
        command.routing.as_ref(),
        command.excel_bridge_enabled,
        revision,
    )
    .await?;
    let audit = crate::mutation_audit(
        context,
        "set_binding",
        "client_api_key",
        command.id.as_str(),
        vec![
            "bound_account_id".to_owned(),
            "excel_bridge_enabled".to_owned(),
            "routing".to_owned(),
        ],
    );
    super::append_admin_audit_event_in_transaction(&mut transaction, audit, revision)
        .await
        .map_err(|e| crate::admin_store_error(RESOURCE, e))?;
    transaction
        .commit()
        .await
        .map_err(|_| error(AdminStoreErrorKind::Unavailable, "commit key binding"))?;
    Ok(ClientKeyBindingMutation {
        config_revision: crate::admin_revision(revision)?,
        binding: ClientKeyBinding {
            id: command.id,
            account_id: command.account_id,
            routing: command.routing,
            excel_bridge_enabled: command.excel_bridge_enabled,
        },
    })
}
