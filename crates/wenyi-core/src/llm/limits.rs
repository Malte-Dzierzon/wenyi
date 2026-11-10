//! Cooperative cancellation, invocation budgets, shared permits.
//! Oracle: `llm/limits.py`.
//!
//! One `RequestLimits` per invocation: connection concurrency caps,
//! per-minute quota windows, request/token budgets, deadline, cancel flag.
//! Permits are never held across retry backoff.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::config::LlmConfig;
use crate::llm::transport::Emit;
use crate::Error;

/// Stop the workflow without model fallback (oracle `RequestStopped`).
#[derive(Debug, Clone)]
pub struct RequestStopped {
    /// Safe message (budgets/cancellation carry no secrets).
    pub message: String,
}

/// Cooperative cancellation (oracle `RequestCancelled`).
pub type RequestCancelled = RequestStopped;

/// One in-flight permit.
#[derive(Debug, Clone)]
pub struct Reservation {
    /// Permit acquisition time.
    pub started: Instant,
    /// Reserved token estimate.
    pub tokens: i64,
    /// Actual tokens after usage arrives.
    pub actual_tokens: Option<i64>,
}

#[derive(Debug, Default)]
struct LimitsInner {
    cancelled: bool,
    active: BTreeMap<String, i64>,
    windows: BTreeMap<String, VecDeque<Reservation>>,
    requests: i64,
    reserved_tokens: i64,
}

/// Shared invocation limits.
pub struct RequestLimits {
    config: LlmConfig,
    started: Instant,
    inner: Mutex<LimitsInner>,
    changed: Condvar,
}

impl RequestLimits {
    /// Bind limits to one invocation's frozen config.
    pub fn new(config: LlmConfig) -> Self {
        Self {
            config,
            started: Instant::now(),
            inner: Mutex::new(LimitsInner::default()),
            changed: Condvar::new(),
        }
    }

    /// Cancel cooperatively; in-flight attempts stop at checkpoints.
    pub fn cancel(&self) {
        let mut inner = self.inner.lock().expect("limits lock");
        inner.cancelled = true;
        self.changed.notify_all();
    }

    /// Raise when cancelled or past deadline (oracle `check`).
    pub fn check(&self) -> std::result::Result<(), RequestStopped> {
        let inner = self.inner.lock().expect("limits lock");
        if inner.cancelled {
            return Err(RequestStopped {
                message: "Model requests cancelled; completed work remains resumable".to_string(),
            });
        }
        if let Some(deadline) = self.config.budget.deadline_seconds {
            if self.started.elapsed().as_secs_f64() >= deadline {
                return Err(RequestStopped {
                    message: "Model request deadline reached; resume with a new deadline"
                        .to_string(),
                });
            }
        }
        Ok(())
    }

    /// Cooperative retry wait without retaining a permit (oracle
    /// `wait_for_retry`).
    pub fn wait_for_retry(&self, delay: Duration) -> std::result::Result<(), RequestStopped> {
        let until = Instant::now() + delay;
        loop {
            self.check()?;
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            let inner = self.inner.lock().expect("limits lock");
            let _ = self
                .changed
                .wait_timeout(inner, remaining.min(Duration::from_millis(100)));
        }
    }

    /// Acquire a permit, waiting on budget/concurrency/quota with
    /// `llm_request_waiting` events (oracle `attempt`). Returns a guard
    /// releasing the permit and truing up the token reservation.
    pub fn acquire(
        &self,
        connection: &str,
        estimate: i64,
        emit: &Emit,
    ) -> std::result::Result<Permit<'_>, RequestStopped> {
        let provider = self
            .config
            .providers
            .get(connection)
            .ok_or_else(|| RequestStopped {
                message: format!("Unknown connection {connection:?}"),
            })?;
        let quota = provider
            .quota_group
            .as_deref()
            .and_then(|g| self.config.quotas.get(g));
        let budget = &self.config.budget;
        if let Some(quota) = quota {
            if let Some(tpm) = quota.tokens_per_minute {
                if estimate > tpm {
                    return Err(RequestStopped {
                        message:
                            "A request exceeds tokens_per_minute; reduce the batch/output limit"
                                .to_string(),
                    });
                }
            }
        }
        let mut last_reason: Option<String> = None;
        loop {
            self.check()?;
            let mut inner = self.inner.lock().expect("limits lock");
            let now = Instant::now();
            let window_key = provider.quota_group.clone().unwrap_or_default();
            {
                let window = inner.windows.entry(window_key.clone()).or_default();
                while window
                    .front()
                    .is_some_and(|r| now.duration_since(r.started) >= Duration::from_secs(60))
                {
                    window.pop_front();
                }
            }
            if let Some(max_requests) = budget.max_requests {
                if inner.requests >= max_requests {
                    return Err(RequestStopped {
                        message: "Model request budget exhausted".to_string(),
                    });
                }
            }
            let reason: Option<String> = if let Some(max_tokens) = budget.max_tokens {
                if inner.reserved_tokens + estimate > max_tokens {
                    if inner.active.values().all(|active| *active == 0) {
                        return Err(RequestStopped {
                            message: "Model token reservation budget exhausted".to_string(),
                        });
                    }
                    Some("token_budget".to_string())
                } else {
                    None
                }
            } else {
                None
            }
            .or_else(|| {
                provider.max_concurrency.and_then(|cap| {
                    (inner.active.get(connection).copied().unwrap_or(0) >= cap)
                        .then(|| "connection_concurrency".to_string())
                })
            })
            .or_else(|| {
                quota.and_then(|quota| {
                    quota.requests_per_minute.and_then(|rpm| {
                        let window = inner.windows.get(&window_key);
                        (window.map(|w| w.len() as i64).unwrap_or(0) >= rpm)
                            .then(|| "requests_per_minute".to_string())
                    })
                })
            })
            .or_else(|| {
                quota.and_then(|quota| {
                    quota.tokens_per_minute.and_then(|tpm| {
                        let windowed: i64 = inner
                            .windows
                            .get(&window_key)
                            .map(|w| w.iter().map(|r| r.tokens).sum())
                            .unwrap_or(0);
                        (windowed + estimate > tpm).then(|| "tokens_per_minute".to_string())
                    })
                })
            });
            if let Some(reason) = reason {
                let changed = Some(reason.clone()) != last_reason;
                if !changed {
                    let (guard, _) = self
                        .changed
                        .wait_timeout(inner, Duration::from_millis(100))
                        .expect("limits lock");
                    drop(guard);
                } else {
                    drop(inner);
                    let mut fields = BTreeMap::new();
                    fields.insert("reason".to_string(), reason.clone());
                    emit("llm_request_waiting", &fields);
                    last_reason = Some(reason);
                }
                continue;
            }
            let reservation = Reservation {
                started: now,
                tokens: estimate,
                actual_tokens: None,
            };
            inner.requests += 1;
            inner.reserved_tokens += estimate;
            *inner.active.entry(connection.to_string()).or_insert(0) += 1;
            if quota.is_some() {
                inner
                    .windows
                    .entry(window_key)
                    .or_default()
                    .push_back(reservation.clone());
            }
            return Ok(Permit {
                limits: self,
                connection: connection.to_string(),
                reservation: std::sync::Mutex::new(Some(reservation)),
            });
        }
    }
}

/// Held permit; releases concurrency + trues up tokens on drop.
pub struct Permit<'a> {
    limits: &'a RequestLimits,
    connection: String,
    reservation: Mutex<Option<Reservation>>,
}

impl Permit<'_> {
    /// True up the reservation with actual billed tokens.
    pub fn settle(&self, actual_tokens: i64) {
        if let Ok(mut slot) = self.reservation.lock() {
            if let Some(reservation) = slot.as_mut() {
                reservation.actual_tokens = Some(actual_tokens);
            }
        }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut inner = self.limits.inner.lock().expect("limits lock");
        if let Ok(mut slot) = self.reservation.lock() {
            if let Some(reservation) = slot.take() {
                if let Some(actual) = reservation.actual_tokens {
                    inner.reserved_tokens += actual - reservation.tokens;
                }
                if let Some(active) = inner.active.get_mut(&self.connection) {
                    *active = (*active - 1).max(0);
                }
                self.limits.changed.notify_all();
            }
        }
    }
}

impl From<RequestStopped> for Error {
    fn from(stopped: RequestStopped) -> Self {
        Error::Stopped(stopped.message)
    }
}
