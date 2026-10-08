//! 定价插件后台任务：关闭浏览器后仍执行，写入复用人工同步校验与审计。
use crate::{
    SettingsService,
    model::{MutationActor, MutationContext, pricing::SyncPricing},
    ports::pricing::PricingSyncPolicy,
};
use chrono::Utc;
use futures::future::BoxFuture;
use gateway_core::task::{ScheduledTask, WorkerCycleContext, WorkerTaskError};
use serde_json::json;
use std::{sync::Arc, time::Duration};

pub struct PricingSyncTask {
    pub settings: Arc<dyn SettingsService>,
    pub policy: Option<Arc<dyn PricingSyncPolicy>>,
}

impl ScheduledTask for PricingSyncTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let Some(policy) = &self.policy else {
                return Ok(());
            };
            let claim = policy
                .decide(json!({"action":"claim","now":Utc::now().timestamp()}))
                .await?;
            if claim.get("run").and_then(serde_json::Value::as_bool) != Some(true) {
                return Ok(());
            }
            let run = claim
                .get("runId")
                .cloned()
                .ok_or_else(|| WorkerTaskError::safe("pricing run missing"))?;
            let result = tokio::time::timeout(Duration::from_secs(120), async {
                let preview = self.settings.preview_pricing_sync().await?;
                let models = preview
                    .prices
                    .iter()
                    .filter(|(_, prices)| !prices.is_empty())
                    .map(|(provider, prices)| (provider.clone(), prices.keys().cloned().collect()))
                    .collect();
                self.settings
                    .sync_pricing(
                        &MutationContext {
                            actor: MutationActor::System,
                            request_id: format!("pricing-daily-{}", Utc::now().timestamp()),
                        },
                        SyncPricing { preview, models },
                    )
                    .await
            })
            .await;
            let success = matches!(result, Ok(Ok(())));
            policy.decide(json!({"action":"finish","now":Utc::now().timestamp(),"runId":run,"success":success})).await?;
            if !success {
                tracing::warn!("scheduled pricing sync failed; keeping existing prices");
            }
            Ok(())
        })
    }
}
