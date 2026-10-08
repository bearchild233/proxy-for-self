-- 管理端覆盖实例入口容量；NULL 继承部署配置，兼容已有安装。
ALTER TABLE runtime_settings ADD COLUMN inference_limits_json jsonb
    CHECK (inference_limits_json IS NULL OR jsonb_typeof(inference_limits_json) = 'object');
