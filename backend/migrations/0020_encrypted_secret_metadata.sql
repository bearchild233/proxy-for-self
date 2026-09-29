-- 明文 Key 不再作为索引材料，摘要维持自定义 Key 的唯一性，前缀仅用于展示。
alter table client_api_keys add column key_lookup_hash text;
alter table client_api_keys add column key_prefix text;
create unique index client_api_keys_lookup_hash_idx on client_api_keys(key_lookup_hash)
where key_lookup_hash is not null;
