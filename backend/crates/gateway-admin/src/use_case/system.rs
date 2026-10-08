//! 系统管理用例。

use std::sync::Arc;

use crate::model::system::{
    LoginProtectionPolicy, LoginProtectionStatus, LoginUnban, PluginAction, PluginStatus,
};
use async_trait::async_trait;

use crate::{
    model::{
        AdminError, AdminErrorKind,
        system::{SystemOperationAccepted, SystemUpdateDetail, SystemUpdateStatus, SystemVersion},
    },
    ports::system::{
        SystemOperationError, SystemOperationErrorKind, SystemOperations, SystemUpdateEventStream,
    },
};

/// API 消费的系统管理服务。
#[async_trait]
pub trait SystemService: Send + Sync {
    async fn plugin_request(
        &self,
        request: crate::model::system::PluginRequest,
    ) -> Result<serde_json::Value, AdminError> {
        if request.kind == "catalog" {
            return Ok(serde_json::json!([]));
        }
        Err(AdminError::new(AdminErrorKind::Conflict, "插件平台未配置"))
    }

    async fn login_protection(&self) -> Result<LoginProtectionStatus, AdminError> {
        Ok(LoginProtectionStatus {
            available: false,
            healthy: false,
            policies: Vec::new(),
            bans: Vec::new(),
            total_bans: 0,
        })
    }
    async fn set_login_protection(
        &self,
        _policy: LoginProtectionPolicy,
    ) -> Result<LoginProtectionStatus, AdminError> {
        Err(AdminError::new(AdminErrorKind::Conflict, "登录防护未配置"))
    }
    async fn unban_login(&self, _request: LoginUnban) -> Result<LoginProtectionStatus, AdminError> {
        Err(AdminError::new(AdminErrorKind::Conflict, "登录防护未配置"))
    }

    async fn plugins(&self) -> Result<Vec<PluginStatus>, AdminError> {
        Ok(Vec::new())
    }
    async fn plugin_action(
        &self,
        _id: String,
        _action: PluginAction,
    ) -> Result<String, AdminError> {
        Err(AdminError::new(AdminErrorKind::Conflict, "插件管理未配置"))
    }

    async fn version(&self) -> Result<SystemVersion, AdminError>;
    async fn update_detail(&self, refresh: bool) -> Result<SystemUpdateDetail, AdminError>;
    fn update_events(&self) -> SystemUpdateEventStream;
    async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<SystemOperationAccepted, AdminError>;
    async fn update_status(&self) -> Result<SystemUpdateStatus, AdminError>;
    async fn rollback(&self) -> Result<SystemOperationAccepted, AdminError>;
    async fn restart(&self) -> Result<SystemOperationAccepted, AdminError>;
}

/// 保持 Host 能力窄边界的默认系统用例。
pub(crate) struct DefaultSystemService {
    operations: Arc<dyn SystemOperations>,
}

impl DefaultSystemService {
    #[must_use]
    pub(crate) const fn new(operations: Arc<dyn SystemOperations>) -> Self {
        Self { operations }
    }
}

#[async_trait]
impl SystemService for DefaultSystemService {
    async fn plugin_request(
        &self,
        request: crate::model::system::PluginRequest,
    ) -> Result<serde_json::Value, AdminError> {
        self.operations
            .plugin_request(request)
            .await
            .map_err(map_system_error)
    }
    async fn login_protection(&self) -> Result<LoginProtectionStatus, AdminError> {
        self.operations
            .login_protection()
            .await
            .map_err(map_system_error)
    }
    async fn set_login_protection(
        &self,
        policy: LoginProtectionPolicy,
    ) -> Result<LoginProtectionStatus, AdminError> {
        if !(3..=20).contains(&policy.max_failures)
            || !(60..=3600).contains(&policy.window_seconds)
            || !(60..=86400).contains(&policy.ban_seconds)
            || !(policy.ban_seconds..=604800).contains(&policy.max_ban_seconds)
        {
            return Err(AdminError::invalid("登录防护阈值不合法"));
        }
        self.operations
            .set_login_protection(policy)
            .await
            .map_err(map_system_error)
    }
    async fn unban_login(&self, request: LoginUnban) -> Result<LoginProtectionStatus, AdminError> {
        self.operations
            .unban_login(request)
            .await
            .map_err(map_system_error)
    }

    async fn plugins(&self) -> Result<Vec<PluginStatus>, AdminError> {
        self.operations.plugins().await.map_err(map_system_error)
    }
    async fn plugin_action(&self, id: String, action: PluginAction) -> Result<String, AdminError> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(AdminError::new(AdminErrorKind::Invalid, "未知插件"));
        }
        self.operations
            .plugin_action(id, action)
            .await
            .map_err(map_system_error)
    }

    async fn version(&self) -> Result<SystemVersion, AdminError> {
        self.operations.version().await.map_err(map_system_error)
    }

    async fn update_detail(&self, refresh: bool) -> Result<SystemUpdateDetail, AdminError> {
        self.operations
            .update_detail(refresh)
            .await
            .map_err(map_system_error)
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        self.operations.update_events()
    }

    async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<SystemOperationAccepted, AdminError> {
        let target_version = target_version
            .map(|version| version.trim().to_owned())
            .filter(|version| !version.is_empty());
        self.operations
            .perform_update(target_version)
            .await
            .map_err(map_system_error)
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, AdminError> {
        self.operations
            .update_status()
            .await
            .map_err(map_system_error)
    }

    async fn rollback(&self) -> Result<SystemOperationAccepted, AdminError> {
        self.operations.rollback().await.map_err(map_system_error)
    }

    async fn restart(&self) -> Result<SystemOperationAccepted, AdminError> {
        self.operations.restart().await.map_err(map_system_error)
    }
}

fn map_system_error(error: SystemOperationError) -> AdminError {
    let kind = match error.kind() {
        SystemOperationErrorKind::Invalid => AdminErrorKind::Invalid,
        SystemOperationErrorKind::Conflict => AdminErrorKind::Conflict,
        SystemOperationErrorKind::Upstream => AdminErrorKind::BadGateway,
        SystemOperationErrorKind::Internal => AdminErrorKind::Internal,
    };
    let message = match kind {
        AdminErrorKind::Invalid => "系统操作请求不合法",
        AdminErrorKind::Conflict => "系统当前状态不允许执行该操作",
        AdminErrorKind::BadGateway => "系统更新服务请求失败",
        AdminErrorKind::Internal => "系统操作失败",
        _ => "系统操作失败",
    };
    AdminError::new(kind, message)
}
