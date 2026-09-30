-- 生命周期事实独立于凭据与基础权重；默认值只影响新增记录。
alter table provider_accounts add column if not exists lifecycle_json jsonb not null default '{}'::jsonb
    check (jsonb_typeof(lifecycle_json) = 'object');
alter table provider_accounts alter column weight set default 50;
