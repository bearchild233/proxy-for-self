-- 每把 Key 记录自身绑定变更的配置版本，避免其他 Key 更新时误清理会话。
alter table client_key_bindings
  add column binding_revision bigint not null default 1
  check (binding_revision > 0);
