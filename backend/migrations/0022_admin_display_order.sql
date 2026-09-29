-- 展示顺序独立于路由配置；未排序的新项排在已保存项之后。
create table admin_display_orders (
    scope text primary key check (scope in ('accounts','groups','keys','proxies')),
    ids text[] not null default '{}',
    updated_at timestamptz not null default now()
);
insert into admin_display_orders(scope, ids) values
 ('accounts', array(select id from provider_accounts order by id)),
 ('groups', array(select id from account_groups order by created_at desc, id desc)),
 ('keys', array(select id from client_api_keys order by created_at desc, id desc)),
 ('proxies', array(select id from outbound_proxies order by name, id));
create function admin_display_rank(list_scope text, item_id text) returns integer
language sql stable parallel safe as $$
 select coalesce((select array_position(ids, item_id) from admin_display_orders where scope = list_scope), 2147483647)
$$;
-- 仅替换旧版本的默认值，保留已有自定义位置与开关。
alter table runtime_settings alter column request_location_json set default
 '{"country":"US","region":"California","city":"Los Angeles","timezone":"America/Los_Angeles"}'::jsonb;
update runtime_settings set request_location_json =
 '{"country":"US","region":"California","city":"Los Angeles","timezone":"America/Los_Angeles"}'::jsonb,
 config_revision = config_revision + 1
 where request_location_json = '{"country":"US","region":"Ohio","city":"Piketon","timezone":"America/New_York"}'::jsonb;
