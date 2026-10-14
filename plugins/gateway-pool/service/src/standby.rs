//! 备用票：窗口用满时只准备一张备用票（状态 `standby`），当前窗口继续服务；当前窗口遇到 29、业务失败、
//! Cookie 将到期或需要换时，备用票立即转为 `ready` 接手。备用票自拿到起超过 `STANDBY_LIFETIME_SECONDS`
//! 仍未启用就丢弃并静置。同一账号同时最多一张备用票。
//!
//! 组合字段：`standbyPlan`（准备中的组合是备用票）、`standbyAt`（拿到备用票的时刻）、`activatedAt`（转正时刻）、
//! `fromStandby`（该窗口由备用票转正）、`windowStartedAt`（转正后第一次业务访问，窗口计时起点）。

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::{
    error::Failure,
    num::Num,
    policy::{self, Cause, STANDBY_LIFETIME_SECONDS},
    runtime::{account_pairs, field, missing},
    store::Store,
    util::{Object, encode, exec, num_field, one, set, sql_of, sql_of_num, str_field, text},
};

fn state_is(pair: &Object, state: &str) -> bool {
    str_field(pair, "state") == Some(state)
}

/// 备用票是否已超过存活期（拿到之后超过 50 分钟）。
pub(crate) fn expired(standby: &Object, now: Num) -> bool {
    num_field(standby, "standbyAt")
        .is_some_and(|at| now.gt(at.plus(Num::Int(STANDBY_LIFETIME_SECONDS))))
}

impl Store {
    /// 备用票到期就丢弃。返回是否丢弃了。
    pub(crate) fn expire_standby(
        &self,
        db: &Connection,
        account: &str,
        settings: &Object,
        now: Num,
    ) -> Result<bool, Failure> {
        let pairs = account_pairs(db, account)?;
        self.expire_standby_in(db, account, settings, now, &pairs)
    }

    /// 同 `expire_standby`，调用方已经读过账号的组合时复用，不再多一次查询。
    pub(crate) fn expire_standby_in(
        &self,
        db: &Connection,
        account: &str,
        settings: &Object,
        now: Num,
        pairs: &[Object],
    ) -> Result<bool, Failure> {
        if !pairs
            .iter()
            .any(|pair| state_is(pair, "standby") && expired(pair, now))
        {
            return Ok(false);
        }
        self.discard_standby(db, account, settings, "standby_expired")
    }

    /// 丢弃账号的备用票并静置：起点是拿到它的时间（最后一次访问上游结束），时长按起始档。
    pub(crate) fn discard_standby(
        &self,
        db: &Connection,
        account: &str,
        settings: &Object,
        reason: &'static str,
    ) -> Result<bool, Failure> {
        let mut discarded = false;
        for mut pair in account_pairs(db, account)? {
            if state_is(&pair, "standby") {
                policy::retire(&mut pair, settings, Cause::Planned(reason));
                policy::settle(&mut pair);
                self.save(db, &pair)?;
                discarded = true;
            }
        }
        Ok(discarded)
    }

    /// 探测 21 且账号已有服务中的窗口：保存配套材料，记录复测恢复的档，组合进入 `standby`。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn publish_standby(
        &self,
        db: &Connection,
        data: &Object,
        account: &str,
        settings: &Object,
        now: Num,
        lease: &Object,
        pair: &mut Object,
    ) -> Result<(), Failure> {
        self.store_material(db, data, account, now, lease, pair)?;
        policy::recover(pair, settings, num_field(pair, "probeTierSeconds"));
        set(pair, "state", json!("standby"));
        set(pair, "standbyAt", now.to_json());
        set(pair, "verifiedAt", now.to_json());
        set(pair, "restUntil", Value::Null);
        Ok(())
    }

    /// 备用票到达时，服务中的窗口若已在 Cookie 到期前的准备期，立即换上。
    pub(crate) fn promote_if_cookie_ending(
        &self,
        db: &Connection,
        settings: &Object,
        account: &str,
        now: Num,
    ) -> Result<bool, Failure> {
        let ending = account_pairs(db, account)?
            .iter()
            .any(|pair| policy::rotation(pair, settings, now).cookie_ending);
        if ending {
            return self.promote_standby(db, settings, account, now, None, "cookie_expiring");
        }
        Ok(false)
    }

    /// 备用票转为服务中的窗口。`previous` 是调用方刚退役的旧窗口（已保存）；没有给出时，
    /// 账号现有的服务中窗口以 `cause` 为原因在此退役。没有可用备用票（不存在、过期、材料丢失）返回 false。
    pub(crate) fn promote_standby(
        &self,
        db: &Connection,
        settings: &Object,
        account: &str,
        now: Num,
        previous: Option<Object>,
        cause: &'static str,
    ) -> Result<bool, Failure> {
        let pairs = account_pairs(db, account)?;
        let Some(mut standby) = pairs.iter().find(|pair| state_is(pair, "standby")).cloned() else {
            return Ok(false);
        };
        if expired(&standby, now) {
            self.discard_standby(db, account, settings, "standby_expired")?;
            return Ok(false);
        }
        let generation = field(&standby, "generation");
        let material = one(
            db,
            "SELECT 1 FROM materials WHERE account=? AND generation=? AND expires>?",
            vec![text(account), sql_of(&generation), sql_of_num(now)],
        )?;
        if material.is_none() {
            self.discard_standby(db, account, settings, "material_lost")?;
            return Ok(false);
        }
        let mut old = previous;
        for mut ready in pairs.into_iter().filter(|pair| state_is(pair, "ready")) {
            policy::retire(&mut ready, settings, Cause::Planned(cause));
            policy::settle(&mut ready);
            self.save(db, &ready)?;
            old = Some(ready);
        }
        let started = num_field(&standby, "preparationStartedAt")
            .ok_or_else(|| missing("preparationStartedAt"))?;
        let obtained = num_field(&standby, "standbyAt").unwrap_or(now);
        let previous_window = old
            .as_ref()
            .and_then(policy::window_start)
            .map(|start| now.minus(start));
        let switch = json!({
            "fromAccount": old.as_ref().map_or(Value::Null, |old| field(old, "accountId")),
            "fromGateway": old.as_ref().map_or(Value::Null, |old| field(old, "gateway")),
            "toGateway": field(&standby, "gateway"),
            "previousWindowSeconds": previous_window.map_or(Value::Null, Num::to_json),
            "preparationSeconds": obtained.minus(started).to_json(),
            "standbySeconds": now.minus(obtained).to_json(),
            "cause": cause,
        });
        exec(
            db,
            "INSERT INTO switches(at,account,data) VALUES (?,?,?)",
            vec![sql_of_num(now), text(account), text(&encode(&switch))],
        )?;
        set(&mut standby, "state", json!("ready"));
        set(&mut standby, "activatedAt", now.to_json());
        set(&mut standby, "fromStandby", json!(true));
        set(&mut standby, "windowStartedAt", Value::Null);
        standby.remove("standbyPlan");
        self.save(db, &standby)?;
        Ok(true)
    }
}
