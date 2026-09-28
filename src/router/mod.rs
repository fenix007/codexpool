//! Router: combo → ступени → стратегия → упорядоченный список кандидатов на запрос.
//! Failover исполняется в proxy (перебор кандидатов до первого байта клиенту); здесь — только план.

use std::sync::Arc;

use chrono::Utc;

use crate::{config::{ComboConfig, ComboStep, Config, Sticky, Strategy}, pool::{AccountId, AccountState, Pool, Scope}};

#[derive(Debug, Clone)]
pub struct Candidate {
    pub account: AccountId,
    pub alias: String,
    pub step: usize,
    pub upstream_model: String,
    pub sticky_hit: bool,
}

pub struct Router {
    cfg: Arc<Config>,
    pool: Arc<Pool>,
}

impl Router {
    pub fn new(cfg: &Arc<Config>, pool: Arc<Pool>) -> Self { Self { cfg: cfg.clone(), pool } }

    /// План на запрос: кандидаты по ступеням; в каждой ступени — порядок по стратегии,
    /// sticky-учётка (если готова) идёт первой. Пустой план → pool_exhausted.
    pub fn plan(&self, combo_name: &str, model: &str, session_id: Option<&str>) -> Vec<Candidate> {
        let combo = self.combo(combo_name);
        let model = self.cfg.resolve_model(model).to_string();
        let scope = Scope::parse(self.cfg.scope_for(&model));
        let sticky = session_id.and_then(|s| self.pool.sticky_get(s));
        let mut out = Vec::new();

        for (i, step) in combo.steps.iter().enumerate() {
            if let Some(m) = &step.models { if !m.only.is_empty() && !m.only.iter().any(|x| x == &model) { continue; } }
            let upstream_model = step.models.as_ref().and_then(|m| m.rewrite.get(&model).cloned()).unwrap_or_else(|| model.clone());
            let ids = self.expand_accounts(step);
            let mut ready = self.pool.ready_among(&ids, scope);
            if ready.is_empty() { continue; }
            order(&mut ready, step.strategy, scope, &self.pool);
            if step.sticky == Sticky::Session {
                if let Some(s) = &sticky {
                    if let Some(pos) = ready.iter().position(|a| &a.id == s) { let a = ready.remove(pos); ready.insert(0, a); }
                }
            }
            for a in ready {
                if out.iter().any(|c: &Candidate| c.account == a.id) { continue; } // одна учётка — один раз в плане (ревью)
                let sticky_hit = sticky.as_ref() == Some(&a.id);
                out.push(Candidate { account: a.id.clone(), alias: a.alias.clone(), step: i, upstream_model: upstream_model.clone(), sticky_hit });
            }
        }
        out.truncate(self.cfg.failover.max_attempts as usize);
        out
    }

    pub fn scope_of(&self, model: &str) -> Scope { Scope::parse(self.cfg.scope_for(self.cfg.resolve_model(model))) }

    fn combo(&self, name: &str) -> &ComboConfig {
        self.cfg.combo.iter().find(|c| c.name == name)
            .or_else(|| self.cfg.combo.iter().find(|c| c.name == self.cfg.default_combo))
            .or_else(|| self.cfg.combo.first())
            .expect("at least one combo in config")
    }

    /// "*" — все; "prefix-*" — по алиасу; "tag:x" — по тегу; иначе точный алиас/id.
    fn expand_accounts(&self, step: &ComboStep) -> Vec<AccountId> {
        let all = self.pool.snapshot();
        let mut ids: Vec<AccountId> = Vec::new();
        for pat in &step.accounts {
            for a in &all {
                let hit = pat == "*"
                    || pat.strip_suffix('*').map_or(false, |p| a.alias.starts_with(p))
                    || pat.strip_prefix("tag:").map_or(false, |t| a.tags.iter().any(|x| x == t))
                    || *pat == a.alias || *pat == a.id;
                if hit && !ids.contains(&a.id) { ids.push(a.id.clone()); }
            }
        }
        ids
    }
}

fn order(ready: &mut [AccountState], strategy: Strategy, scope: Scope, pool: &Pool) {
    let now = Utc::now();
    match strategy {
        Strategy::QuotaDeadline => {
            let weights: Vec<f64> = ready.iter().map(|a| quota_deadline_weight(a, scope, now)).collect();
            let mut idx: Vec<usize> = (0..ready.len()).collect();
            idx.sort_by(|&i, &j| {
                let idle = |a: &AccountState| a.last_used_at.map_or(24.0 * 3600.0, |t| (now - t).num_seconds().max(0) as f64);
                let (di, dj) = (idle(&ready[i]) * weights[i], idle(&ready[j]) * weights[j]);
                dj.partial_cmp(&di).unwrap_or(std::cmp::Ordering::Equal)
                    .then(weights[j].partial_cmp(&weights[i]).unwrap_or(std::cmp::Ordering::Equal))
                    .then(ready[j].priority.cmp(&ready[i].priority))
            });
            let sorted: Vec<AccountState> = idx.into_iter().map(|i| ready[i].clone()).collect();
            ready.clone_from_slice(&sorted);
        }
        Strategy::Priority => ready.sort_by_key(|a| (-a.priority, a.alias.clone())),
        Strategy::RoundRobin => { let k = pool.next_round_robin() % ready.len().max(1); ready.rotate_left(k); }
        Strategy::LeastInflight => ready.sort_by_key(|a| a.scopes.get(&scope).map_or(0, |s| s.inflight)),
        // Ключ — минимум из двух окон: учётка 99 % / 1 % не должна обгонять 50 % / 50 % (ревью).
        Strategy::Headroom => ready.sort_by(|a, b| {
            let h = |x: &AccountState| x.scopes.get(&scope).map(|s| s.window_5h.headroom(now).min(s.window_weekly.headroom(now))).unwrap_or(100.0);
            h(b).partial_cmp(&h(a)).unwrap_or(std::cmp::Ordering::Equal)
        }),
        Strategy::ResetSoonest => ready.sort_by_key(|a| a.scopes.get(&scope).and_then(|s| s.window_5h.reset_at).map(|t| t.timestamp()).unwrap_or(i64::MAX)),
    }
}

/// Вес стратегии quota_deadline — порт `codexQuotaDeadlineRouting.ts` из stable-форка (d5e638b40, 0f092f480).
/// weight = min(6, 1 + rate/20), где rate — сколько процентов недельного окна в сутки нужно «сжечь»
/// до дедлайна (reset недельного окна). Reset credits и subscription_end добавляются в MVP-2
/// (нужны поля в AccountState). Неизвестная неделя → вес 1 (в stable — медиана пула; TODO).
fn quota_deadline_weight(a: &AccountState, scope: Scope, now: chrono::DateTime<Utc>) -> f64 {
    let Some(s) = a.scopes.get(&scope) else { return 1.0 };
    let remaining = (s.window_weekly.headroom(now) as f64 - 5.0).max(0.0);
    // Прошедший reset — окно уже сброшено: горизонт 7 дней, как в stable (ревью: раньше считалось «дедлайн через 6 ч»).
    let days = match s.window_weekly.reset_at {
        Some(r) if r > now => ((r - now).num_seconds() as f64 / 86_400.0).max(0.25),
        _ => 7.0,
    };
    let rate = remaining / days;
    (1.0 + rate / 20.0).min(6.0)
}
