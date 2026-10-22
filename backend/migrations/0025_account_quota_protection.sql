-- 独立保存养号政策，旧版本不会解析新增列，真实额度事实不变。
ALTER TABLE provider_accounts ADD COLUMN quota_protection_json jsonb NOT NULL
    DEFAULT '{"enabled":false,"usedPercentLimit":90}'::jsonb;
ALTER TABLE provider_accounts ADD CONSTRAINT provider_accounts_quota_protection_valid CHECK (
    jsonb_typeof(quota_protection_json) = 'object'
    AND jsonb_typeof(quota_protection_json->'enabled') = 'boolean'
    AND jsonb_typeof(quota_protection_json->'usedPercentLimit') = 'number'
    AND (quota_protection_json->>'usedPercentLimit')::numeric BETWEEN 1 AND 99
    AND (quota_protection_json->>'usedPercentLimit')::numeric = trunc((quota_protection_json->>'usedPercentLimit')::numeric)
    AND quota_protection_json ? 'enabled' AND quota_protection_json ? 'usedPercentLimit'
);
