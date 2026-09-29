-- 保留旧 Key 的固定账号范围，新增 Key 可显式选择账号池或动态分组。
alter table provider_accounts add column name_customized boolean not null default false;
alter table client_key_bindings add column routing_options jsonb;
alter table client_key_bindings add constraint key_routing_options_object_ck check (routing_options is null or jsonb_typeof(routing_options) = 'object');
alter table model_requests drop constraint model_requests_routing_scope_ck;
alter table model_requests add constraint model_requests_routing_scope_ck check (routing_scope in ('legacy_provider', 'all', 'groups', 'account', 'accounts', 'denied'));
alter table model_requests drop constraint model_requests_routing_group_names_ck;
alter table model_requests add constraint model_requests_routing_group_names_ck check (
 jsonb_typeof(routing_group_names_snapshot) = 'array' and (
 (routing_scope in ('legacy_provider', 'all', 'denied') and cardinality(routing_group_refs)=0 and jsonb_array_length(routing_group_names_snapshot)=0)
 or (routing_scope='groups' and cardinality(routing_group_refs)>0 and array_position(routing_group_refs,null) is null and jsonb_array_length(routing_group_names_snapshot)=cardinality(routing_group_refs))
 or (routing_scope in ('account','accounts') and cardinality(routing_group_refs)>0 and (routing_scope='accounts' or cardinality(routing_group_refs)=1) and array_position(routing_group_refs,null) is null and to_jsonb(routing_group_refs)=routing_group_names_snapshot)
 ));
