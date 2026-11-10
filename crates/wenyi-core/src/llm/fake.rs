//! Deterministic offline provider. Oracle: `llm/providers/fake.py`.
//!
//! No SDK, no credentials, no network. `FakeProvider` is the transport
//! used through the real retry/limits/router path; `FakeClient` injects
//! directly into workflows with scripted handlers and call recording.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use crate::config::LlmConfig;
use crate::llm::compat::Message;
use crate::llm::errors::ProviderError;
use crate::llm::operations::require_operation;
use crate::llm::providers::ProviderKind;
use crate::llm::routing::{model_route, resolve_routes, ResolvedRoute};
use crate::llm::transport::{Provider, RequestContext};
use crate::Result;

/// Transport with injectable behavior and no credentials.
#[derive(Debug, Default)]
pub struct FakeProvider {
    /// Scripted response per call index (fallback: `"[]"`/`""`).
    pub script: Vec<String>,
    /// Calls served so far.
    pub calls: Mutex<usize>,
}

impl FakeProvider {
    /// Unscripted provider (oracle defaults).
    pub fn new() -> Self {
        Self::default()
    }
}

impl Provider for FakeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Fake
    }

    fn endpoint(&self) -> Option<&str> {
        None
    }

    fn validate_credentials(&self) -> Result<()> {
        Ok(())
    }

    fn request<'a>(
        &'a self,
        _messages: &'a [Message],
        _model: &'a crate::llm::transport::ResolvedModel,
        json_mode: bool,
        _context: &'a RequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<String, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let mut calls = self.calls.lock().expect("fake calls");
            let index = *calls;
            *calls += 1;
            Ok(self.script.get(index).cloned().unwrap_or_else(|| {
                if json_mode {
                    "[]".to_string()
                } else {
                    String::new()
                }
            }))
        })
    }

    fn list_models<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, ProviderError>> + Send + 'a>> {
        Box::pin(async move { Ok(vec!["fake".to_string()]) })
    }
}

/// Scripted handler: (messages, tier, json_mode) -> text.
pub type FakeHandler = Box<dyn Fn(&[Message], &str, bool) -> String + Send + Sync>;

/// One recorded fake call (immutable snapshot).
#[derive(Debug, Clone)]
pub struct FakeCall {
    /// Request messages.
    pub messages: Vec<Message>,
    /// Operation ID.
    pub operation: String,
    /// Routing tier.
    pub tier: String,
    /// Whether JSON was requested.
    pub json_mode: bool,
    /// Effective output cap.
    pub max_tokens: Option<i64>,
    /// Model identifier.
    pub model: String,
    /// Connection name.
    pub provider: String,
}

/// Workflow-injectable fake client with call recording (oracle `FakeClient`).
pub struct FakeClient {
    /// Optional scripted handler.
    pub handler: Option<FakeHandler>,
    /// Frozen config.
    pub config: LlmConfig,
    /// Compiled routes.
    pub routes: BTreeMap<String, ResolvedRoute>,
    /// Recorded calls.
    pub calls: Vec<FakeCall>,
}

impl FakeClient {
    /// Build with an optional handler and config (default: `fake` preset).
    pub fn new(handler: Option<FakeHandler>, config: Option<LlmConfig>) -> Result<Self> {
        let config = match config {
            Some(config) => config,
            None => crate::config::load("llm:\n  preset: fake\n")?.llm,
        };
        let routes = resolve_routes(&config)?;
        Ok(Self {
            handler,
            config,
            routes,
            calls: Vec::new(),
        })
    }

    /// Complete one operation, recording the call.
    pub fn complete(
        &mut self,
        messages: &[Message],
        operation: &str,
        json_mode: bool,
        max_tokens: Option<i64>,
    ) -> Result<String> {
        require_operation(operation)?;
        let base = self.routes.get(operation).expect("resolved route").clone();
        let route = match max_tokens {
            Some(hint) => model_route(
                &self.config,
                operation,
                &base.profile,
                base.origin.clone(),
                base.tier.clone(),
                Vec::new(),
                Some(hint),
            )?,
            None => base,
        };
        let tier = route.tier.clone().unwrap_or_else(|| "direct".to_string());
        self.calls.push(FakeCall {
            messages: messages.to_vec(),
            operation: operation.to_string(),
            tier: tier.clone(),
            json_mode,
            max_tokens: route.max_output_tokens,
            model: route.model.clone(),
            provider: route.provider.clone(),
        });
        Ok(match &self.handler {
            Some(handler) => handler(messages, &tier, json_mode),
            None => {
                if json_mode {
                    "[]".to_string()
                } else {
                    String::new()
                }
            }
        })
    }
}
