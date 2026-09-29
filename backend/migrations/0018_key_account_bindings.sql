-- Key 绑定与插件开关独立持久化；账号删除后只撤销绑定，绝不扩大权限。
create table client_key_bindings (
  client_api_key_id text primary key references client_api_keys(id) on delete cascade,
  provider_account_id text references provider_accounts(id) on delete set null,
  excel_bridge_enabled boolean not null default false,
  updated_at timestamptz not null default now()
);

create index client_key_bindings_account_idx
  on client_key_bindings(provider_account_id)
  where provider_account_id is not null;

alter table model_requests drop constraint model_requests_routing_scope_ck;
alter table model_requests add constraint model_requests_routing_scope_ck check (
  routing_scope in ('legacy_provider', 'all', 'groups', 'account', 'denied')
);

-- 保留历史列名；account 范围在 refs/names 中只保存一个固定账号引用。
alter table model_requests drop constraint model_requests_routing_group_names_ck;
alter table model_requests add constraint model_requests_routing_group_names_ck check (
  jsonb_typeof(routing_group_names_snapshot) = 'array'
  and (
    (
      routing_scope in ('legacy_provider', 'all', 'denied')
      and cardinality(routing_group_refs) = 0
      and jsonb_array_length(routing_group_names_snapshot) = 0
    )
    or (
      routing_scope = 'groups'
      and cardinality(routing_group_refs) > 0
      and array_position(routing_group_refs, null) is null
      and jsonb_array_length(routing_group_names_snapshot) = cardinality(routing_group_refs)
    )
    or (
      routing_scope = 'account'
      and cardinality(routing_group_refs) = 1
      and left(routing_group_refs[1], 5) = 'acct_'
      and jsonb_array_length(routing_group_names_snapshot) = 1
      and routing_group_names_snapshot ->> 0 = routing_group_refs[1]
    )
  )
);
