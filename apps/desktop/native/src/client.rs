//! Loopback REST client for the Python sidecar.
//!
//! Mirrors `apps/desktop/frontend/src/transport.ts`: `${apiBase}${path}` +
//! bearer token. All calls are async and must run off the UI thread
//! (via `cx.background_spawn` + entity update, or the shared tokio runtime).

use serde::de::DeserializeOwned;
use std::future::Future;

/// Shared Tokio runtime for reqwest I/O. GPUI's executors are not Tokio
/// runtimes, so every client call below runs inside `block_on`. Callers must
/// invoke these from a background thread (never the GPUI UI thread).
static RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> =
    std::sync::LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    });

/// Run a client future to completion on the shared runtime.
/// Must be called off the UI thread.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    RUNTIME.block_on(fut)
}
#[derive(Debug, Clone)]
pub struct Client {
    inner: reqwest::Client,
    pub api_base: String,
    pub token: String,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("api error {status}: {detail}")]
    Api { status: u16, detail: String },
}

impl Client {
    pub fn new(api_base: String, token: String) -> Self {
        Self {
            inner: reqwest::Client::new(),
            api_base: api_base.trim_end_matches('/').to_string(),
            token,
        }
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, Error> {
        let mut req = self
            .inner
            .request(method, format!("{}{}", self.api_base, path))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().await.unwrap_or_default();
            // Never log the token; the detail comes from the local backend.
            return Err(Error::Api {
                status: status.as_u16(),
                detail: detail.chars().take(300).collect(),
            });
        }
        Ok(resp.json().await?)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.request(reqwest::Method::GET, path, None).await
    }

    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T, Error> {
        self.request(reqwest::Method::POST, path, Some(body)).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.request(reqwest::Method::DELETE, path, None).await
    }

    // -- Dashboard ops (doc 18 §2 screen 1) --

    /// `GET /projects` — mirrors `api.listProjects`.
    pub async fn list_projects(&self) -> Result<Vec<Project>, Error> {
        self.get("/projects").await
    }

    /// `DELETE /projects/{pid}` — mirrors `api.deleteProject`.
    pub async fn delete_project(&self, pid: &str) -> Result<serde_json::Value, Error> {
        self.delete(&format!("/projects/{pid}")).await
    }

    /// `GET /health` — unauthenticated liveness probe.
    pub async fn health(&self) -> Result<serde_json::Value, Error> {
        self.get("/health").await
    }

    // -- Create project ops (doc 18 §2 screen 2) --

    /// `GET /capabilities` — mirrors `api.capabilities`.
    pub async fn capabilities(&self) -> Result<Capabilities, Error> {
        self.get("/capabilities").await
    }

    /// `GET /projects/{pid}` — mirrors `api.getProject`.
    pub async fn get_project(&self, pid: &str) -> Result<ProjectDetail, Error> {
        self.get(&format!("/projects/{pid}")).await
    }

    /// `GET /projects/{pid}/preview` — mirrors `api.getPreview`.
    /// 409 while the parse is still running (preview not ready).
    pub async fn get_preview(&self, pid: &str) -> Result<UploadPreview, Error> {
        self.get(&format!("/projects/{pid}/preview")).await
    }

    /// `POST /projects` multipart — mirrors `api.createProject`.
    /// Parts: `project` = JSON body, `file` = source bytes.
    pub async fn create_project(
        &self,
        body: &ProjectCreate,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<ProjectDetail, Error> {
        let form = reqwest::multipart::Form::new()
            .text("project", serde_json::to_string(body).unwrap_or_default())
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string()),
            );
        let resp = self
            .inner
            .post(format!("{}/projects", self.api_base))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                detail: detail.chars().take(300).collect(),
            });
        }
        Ok(resp.json().await?)
    }

    /// `POST /projects/{pid}/resume` — mirrors `api.resume`.
    pub async fn resume(&self, pid: &str) -> Result<serde_json::Value, Error> {
        self.post(&format!("/projects/{pid}/resume"), serde_json::json!({})).await
    }

    /// `POST /projects/{pid}/translate` — mirrors `api.translate`.
    pub async fn translate(&self, pid: &str) -> Result<serde_json::Value, Error> {
        self.post(&format!("/projects/{pid}/translate"), serde_json::json!({})).await
    }

    // -- Progress screen ops (doc 18 §2 screen 3) --

    /// `GET /projects/{pid}/chapters` — mirrors `api.listChapters`.
    pub async fn list_chapters(&self, pid: &str) -> Result<Vec<ChapterSummary>, Error> {
        self.get(&format!("/projects/{pid}/chapters")).await
    }

    /// `GET /projects/{pid}/stats` — mirrors `api.getStats`.
    pub async fn get_stats(&self, pid: &str) -> Result<ProjectStats, Error> {
        self.get(&format!("/projects/{pid}/stats")).await
    }

    /// `GET /projects/{pid}/workflow` — mirrors `api.getWorkflow`.
    pub async fn get_workflow(&self, pid: &str) -> Result<WorkflowOut, Error> {
        self.get(&format!("/projects/{pid}/workflow")).await
    }

    /// `GET /projects/{pid}/report` — mirrors `api.getReport`.
    pub async fn get_report(&self, pid: &str) -> Result<serde_json::Value, Error> {
        self.get(&format!("/projects/{pid}/report")).await
    }

    /// `POST /projects/{pid}/report` — mirrors `api.regenerateReport`.
    pub async fn regenerate_report(&self, pid: &str) -> Result<serde_json::Value, Error> {
        self.post(&format!("/projects/{pid}/report"), serde_json::json!({})).await
    }

    /// `POST /projects/{pid}/pause` — mirrors `api.pause`.
    pub async fn pause(&self, pid: &str) -> Result<serde_json::Value, Error> {
        self.post(&format!("/projects/{pid}/pause"), serde_json::json!({})).await
    }

    /// `POST /projects/{pid}/chapters/{ci}/translate` — mirrors `api.translateChapter`.
    pub async fn translate_chapter(&self, pid: &str, ci: u32) -> Result<serde_json::Value, Error> {
        self.post(
            &format!("/projects/{pid}/chapters/{ci}/translate"),
            serde_json::json!({}),
        )
        .await
    }

    // -- Glossary ops (doc 18 §2 screen 4) --

    /// `GET …/glossary/terms?q=&type=` — mirrors `api.listTerms`.
    pub async fn list_terms(
        &self,
        pid: &str,
        q: Option<&str>,
        term_type: Option<&str>,
    ) -> Result<Vec<GlossaryTerm>, Error> {
        let mut path = format!("/projects/{pid}/glossary/terms");
        let mut sep = '?';
        if let Some(q) = q.filter(|q| !q.is_empty()) {
            path.push_str(&format!("{sep}q={}", urlencode(q)));
            sep = '&';
        }
        if let Some(t) = term_type.filter(|t| !t.is_empty()) {
            path.push_str(&format!("{sep}type={}", urlencode(t)));
        }
        self.get(&path).await
    }

    /// `POST …/glossary/terms` — mirrors `api.addTerm`.
    pub async fn add_term(&self, pid: &str, term: &TermIn) -> Result<GlossaryTerm, Error> {
        self.request(
            reqwest::Method::POST,
            &format!("/projects/{pid}/glossary/terms"),
            Some(serde_json::to_value(term).unwrap_or_default()),
        )
        .await
    }

    /// `PUT …/glossary/terms/{source}` — mirrors `api.updateTerm`.
    pub async fn update_term(
        &self,
        pid: &str,
        source: &str,
        term: &TermIn,
    ) -> Result<GlossaryTerm, Error> {
        self.request(
            reqwest::Method::PUT,
            &format!("/projects/{pid}/glossary/terms/{}", urlencode(source)),
            Some(serde_json::to_value(term).unwrap_or_default()),
        )
        .await
    }

    /// `DELETE …/glossary/terms/{source}` — mirrors `api.deleteTerm`.
    pub async fn delete_term(&self, pid: &str, source: &str) -> Result<serde_json::Value, Error> {
        self.delete(&format!("/projects/{pid}/glossary/terms/{}", urlencode(source))).await
    }

    /// `GET …/glossary/conflicts` — mirrors `api.listConflicts`.
    pub async fn list_conflicts(&self, pid: &str) -> Result<Vec<TermConflict>, Error> {
        self.get(&format!("/projects/{pid}/glossary/conflicts")).await
    }

    /// `POST …/glossary/conflicts/{cid}/resolve` — mirrors `api.resolveConflict`.
    pub async fn resolve_conflict(
        &self,
        pid: &str,
        cid: i64,
        decision: &str,
        target: Option<&str>,
    ) -> Result<serde_json::Value, Error> {
        self.post(
            &format!("/projects/{pid}/glossary/conflicts/{cid}/resolve"),
            serde_json::json!({"decision": decision, "target": target}),
        )
        .await
    }

    /// `GET …/glossary/export?format=` — raw bytes (CSV/JSON download).
    pub async fn export_glossary(&self, pid: &str, format: &str) -> Result<Vec<u8>, Error> {
        let resp = self
            .inner
            .get(format!("{}/projects/{pid}/glossary/export?format={format}", self.api_base))
            .bearer_auth(&self.token)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                detail: detail.chars().take(300).collect(),
            });
        }
        Ok(resp.bytes().await.map(|b| b.to_vec())?)
    }

    /// `POST …/glossary/import` — mirrors `api.importGlossary`.
    pub async fn import_glossary(
        &self,
        pid: &str,
        terms: &[TermIn],
    ) -> Result<serde_json::Value, Error> {
        self.post(
            &format!("/projects/{pid}/glossary/import"),
            serde_json::json!({"terms": terms}),
        )
        .await
    }

    // -- Contents ops (doc 18 §2 screen 6) --

    /// `PUT …/chapters/{ci}/title` — mirrors title edit.
    /// Body: `{title_translated, expected_title_translated?}` (stale guard).
    pub async fn update_chapter_title(
        &self,
        pid: &str,
        ci: u32,
        title: &str,
        expected: Option<&str>,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::PUT,
            &format!("/projects/{pid}/chapters/{ci}/title"),
            Some(serde_json::json!({
                "title_translated": title,
                "expected_title_translated": expected,
            })),
        )
        .await
    }

    // -- Proofreading ops (doc 18 §2 screen 8) --

    /// `GET …/review/{ci}` — mirrors `api.getReview` (segments + issues).
    pub async fn get_review_chapter(&self, pid: &str, ci: u32) -> Result<ChapterSegments, Error> {
        self.get(&format!("/projects/{pid}/review/{ci}")).await
    }

    /// `PUT …/review/{ci}/segments/{si}` — mirrors `api.editSegment`.
    pub async fn edit_segment(
        &self,
        pid: &str,
        ci: u32,
        si: u32,
        target: &str,
        expected: Option<&str>,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::PUT,
            &format!("/projects/{pid}/review/{ci}/segments/{si}"),
            Some(serde_json::json!({"target": target, "expected_target": expected})),
        )
        .await
    }

    /// `GET …/review/{ci}/segments/{si}/history` — mirrors `api.segmentHistory`.
    pub async fn segment_history(
        &self,
        pid: &str,
        ci: u32,
        si: u32,
    ) -> Result<Vec<SegmentRevision>, Error> {
        self.get(&format!("/projects/{pid}/review/{ci}/segments/{si}/history")).await
    }

    /// `GET …/chapters/{ci}/segments/{si}/precision-drafts` — mirrors `api.precisionDrafts`.
    pub async fn precision_drafts(
        &self,
        pid: &str,
        ci: u32,
        si: u32,
    ) -> Result<serde_json::Value, Error> {
        self.get(&format!(
            "/projects/{pid}/chapters/{ci}/segments/{si}/precision-drafts"
        ))
        .await
    }

    // -- Review ops (doc 18 §2 screen 7) --

    /// `POST …/review/run` — mirrors `api.runAiReview`.
    pub async fn run_ai_review(&self, pid: &str) -> Result<serde_json::Value, Error> {
        self.post(&format!("/projects/{pid}/review/run"), serde_json::json!({})).await
    }

    /// `GET …/review/runs` — mirrors `api.listReviewRuns`.
    pub async fn list_review_runs(&self, pid: &str) -> Result<Vec<ReviewRun>, Error> {
        self.get(&format!("/projects/{pid}/review/runs")).await
    }

    /// `GET …/review/runs/{rid}` — mirrors `api.getReviewRun`.
    pub async fn get_review_run(&self, pid: &str, rid: &str) -> Result<ReviewRun, Error> {
        self.get(&format!("/projects/{pid}/review/runs/{rid}")).await
    }

    // -- Style ops (doc 18 §2 screen 5) --

    /// `GET /projects/{pid}/analysis` — mirrors `api.getAnalysis`.
    pub async fn get_analysis(&self, pid: &str) -> Result<AnalysisOut, Error> {
        self.get(&format!("/projects/{pid}/analysis")).await
    }

    /// `PUT /projects/{pid}/analysis` — mirrors `api.updateAnalysis`.
    /// Body: `{analysis: {key: value}}`.
    pub async fn update_analysis(
        &self,
        pid: &str,
        analysis: serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::PUT,
            &format!("/projects/{pid}/analysis"),
            Some(serde_json::json!({"analysis": analysis})),
        )
        .await
    }

    /// `PUT /projects/{pid}/chapter-digests/{ci}` — mirrors `api.updateDigest`.
    pub async fn update_digest(
        &self,
        pid: &str,
        ci: u32,
        digest: &str,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::PUT,
            &format!("/projects/{pid}/chapter-digests/{ci}"),
            Some(serde_json::json!({"digest": digest})),
        )
        .await
    }

    // -- Subtitle ops (doc 18 §2 screen 10) --

    /// `GET …/subtitles` — mirrors `api.getSubtitles`.
    pub async fn get_subtitles(&self, pid: &str) -> Result<SubtitleList, Error> {
        self.get(&format!("/projects/{pid}/subtitles")).await
    }

    /// `PUT …/subtitles/{cueid}` — mirrors `api.editSubtitle`.
    pub async fn edit_subtitle(
        &self,
        pid: &str,
        cueid: &str,
        target: &str,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::PUT,
            &format!("/projects/{pid}/subtitles/{}", urlencode(cueid)),
            Some(serde_json::json!({"target": target})),
        )
        .await
    }

    // -- Export ops (doc 18 §2 screen 11) --

    /// `GET …/exports` — mirrors `api.listExports`.
    pub async fn list_exports(&self, pid: &str) -> Result<Vec<ExportOut>, Error> {
        self.get(&format!("/projects/{pid}/exports")).await
    }

    /// `POST …/exports` — mirrors `api.createExport`.
    pub async fn create_export(
        &self,
        pid: &str,
        request: &ExportRequest,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::POST,
            &format!("/projects/{pid}/exports"),
            Some(serde_json::to_value(request).unwrap_or_default()),
        )
        .await
    }

    /// `GET …/exports/{id}/download` — raw bytes.
    pub async fn download_export(&self, pid: &str, id: i64) -> Result<Vec<u8>, Error> {
        let resp = self
            .inner
            .get(format!("{}/projects/{pid}/exports/{id}/download", self.api_base))
            .bearer_auth(&self.token)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                detail: detail.chars().take(300).collect(),
            });
        }
        Ok(resp.bytes().await.map(|b| b.to_vec())?)
    }

    /// `POST /desktop/projects/{pid}/exports/plan` — native save planning.
    pub async fn plan_native_export(
        &self,
        pid: &str,
        request: &ExportRequest,
    ) -> Result<NativeExportPlan, Error> {
        self.request(
            reqwest::Method::POST,
            &format!("/desktop/projects/{pid}/exports/plan"),
            Some(serde_json::to_value(request).unwrap_or_default()),
        )
        .await
    }

    /// `GET /desktop/projects/{pid}/exports/{id}/content` — bytes for native save.
    pub async fn native_export_content(&self, pid: &str, id: i64) -> Result<Vec<u8>, Error> {
        let resp = self
            .inner
            .get(format!(
                "{}/desktop/projects/{pid}/exports/{id}/content",
                self.api_base
            ))
            .bearer_auth(&self.token)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                detail: detail.chars().take(300).collect(),
            });
        }
        Ok(resp.bytes().await.map(|b| b.to_vec())?)
    }

    // -- Events ops (doc 18 §2 screen 12) --

    /// `GET …/events` — mirrors `api.listEvents` (newest first, UI passes no filter).
    pub async fn list_events(&self, pid: &str) -> Result<Vec<EventOut>, Error> {
        self.get(&format!("/projects/{pid}/events")).await
    }

    // -- Project config ops (doc 18 §2 screen 9) --

    /// `GET …/config` — mirrors `api.getConfig`.
    pub async fn get_config(&self, pid: &str) -> Result<ProjectConfigOut, Error> {
        self.get(&format!("/projects/{pid}/config")).await
    }

    /// `PUT …/config` — mirrors `api.saveConfig`. Body: `{yaml}`.
    pub async fn save_config(&self, pid: &str, yaml: &str) -> Result<ProjectConfigOut, Error> {
        self.request(
            reqwest::Method::PUT,
            &format!("/projects/{pid}/config"),
            Some(serde_json::json!({"yaml": yaml})),
        )
        .await
    }

    /// `POST …/config/validate` — mirrors `api.validateConfig`.
    pub async fn validate_config(
        &self,
        pid: &str,
        yaml: &str,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::POST,
            &format!("/projects/{pid}/config/validate"),
            Some(serde_json::json!({"yaml": yaml})),
        )
        .await
    }

    /// `GET …/config/defaults` — mirrors `api.getProjectDefaults`.
    pub async fn get_project_defaults(&self, pid: &str) -> Result<ProjectConfigOut, Error> {
        self.get(&format!("/projects/{pid}/config/defaults")).await
    }

    /// `GET …/models` — mirrors `api.modelRoutes`.
    pub async fn model_routes(&self, pid: &str) -> Result<Vec<serde_json::Value>, Error> {
        self.get(&format!("/projects/{pid}/models")).await
    }

    /// `POST …/models/check` — mirrors `api.checkModels`.
    pub async fn check_models(
        &self,
        pid: &str,
        workflow: &str,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::POST,
            &format!("/projects/{pid}/models/check"),
            Some(serde_json::json!({"workflow": workflow})),
        )
        .await
    }

    // -- Global config ops (doc 18 §2 screen 13) --

    /// `GET /settings` — mirrors `api.getGlobalConfig`.
    pub async fn get_global_config(&self) -> Result<GlobalConfigOut, Error> {
        self.get("/settings").await
    }

    /// `PUT /settings` — mirrors `api.saveGlobalConfig`.
    pub async fn save_global_config(
        &self,
        yaml: &str,
        revision: i64,
    ) -> Result<GlobalConfigOut, Error> {
        self.request(
            reqwest::Method::PUT,
            "/settings",
            Some(serde_json::json!({
                "yaml": yaml,
                "revision": revision,
                "default_template": "标准翻译",
            })),
        )
        .await
    }

    /// `POST /settings/validate` — mirrors `api.validateGlobalConfig`.
    pub async fn validate_global_config(&self, yaml: &str) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::POST,
            "/settings/validate",
            Some(serde_json::json!({"yaml": yaml})),
        )
        .await
    }

    /// `GET /settings/defaults` — mirrors `api.getGlobalDefaults`.
    pub async fn get_global_defaults(&self) -> Result<GlobalConfigOut, Error> {
        self.get("/settings/defaults").await
    }

    /// `POST /settings/models/ping` — mirrors `api.pingModels`.
    pub async fn ping_models(&self, timeout: f32) -> Result<Vec<serde_json::Value>, Error> {
        self.request(
            reqwest::Method::POST,
            "/settings/models/ping",
            Some(serde_json::json!({"timeout": timeout})),
        )
        .await
    }

    /// `POST /settings/models/list` — mirrors model-name fetch.
    pub async fn list_models_remote(
        &self,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        self.request(reqwest::Method::POST, "/settings/models/list", Some(body)).await
    }

    // -- Desktop credentials (settings screens) --

    /// `GET /desktop/credentials` — connection statuses.
    pub async fn credential_statuses(&self) -> Result<serde_json::Value, Error> {
        self.get("/desktop/credentials").await
    }

    /// `PUT /desktop/credentials/{connection}` — save or clear a key.
    pub async fn save_credential(
        &self,
        connection: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::PUT,
            &format!("/desktop/credentials/{}", urlencode(connection)),
            Some(body),
        )
        .await
    }

    /// `GET /desktop/external-credentials/mineru` — MinerU status.
    pub async fn mineru_status(&self) -> Result<serde_json::Value, Error> {
        self.get("/desktop/external-credentials/mineru").await
    }

    /// `PUT /desktop/external-credentials/mineru` — save or clear MinerU key.
    pub async fn save_mineru(&self, body: serde_json::Value) -> Result<serde_json::Value, Error> {
        self.request(
            reqwest::Method::PUT,
            "/desktop/external-credentials/mineru",
            Some(body),
        )
        .await
    }
}
/// Minimal percent-encoding for path/query segments (no extra dep).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Project list item — mirrors the `Project` schema in `api.d.ts`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub title: Option<String>,
    pub fmt: Option<String>,
    pub source_lang: Option<String>,
    pub target_lang: Option<String>,
    pub status: String,
    pub error: Option<String>,
    #[serde(default)]
    pub initialized: bool,
    pub created_at: Option<String>,
}

/// Active statuses — mirrors `ACTIVE_STATUSES` in `lib/api.ts`.
pub const ACTIVE_STATUSES: &[&str] = &[
    "parsing",
    "queued",
    "preparing",
    "translating",
    "reviewing",
    "autofixing",
    "postprocessing",
    "pausing",
];

pub fn is_project_busy(status: &str) -> bool {
    ACTIVE_STATUSES.contains(&status)
}

/// `GET /capabilities` — mirrors `configuration.capabilities`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Capabilities {
    pub languages: Vec<Language>,
    pub input_formats: Vec<String>,
    pub output_formats: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Language {
    pub code: String,
    pub name: String,
}

/// `POST /projects` body — mirrors `ProjectCreate` in `schemas.py`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectCreate {
    pub name: String,
    pub source_lang: String,
    pub target_lang: String,
    #[serde(default)]
    pub strategy: serde_json::Value,
    pub prepare: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_backend: Option<String>,
    pub translation_mode: String,
}

/// Project detail — `Project` plus parsed/preview fields the detail
/// endpoints return (`ProjectDetail` in `api.d.ts`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProjectDetail {
    #[serde(flatten)]
    pub project: Project,
    #[serde(default)]
    pub book_title: Option<String>,
    #[serde(default)]
    pub chapter_count: u32,
    #[serde(default)]
    pub total_word_count: u64,
    #[serde(default)]
    pub done_chapters: u32,
}

/// `GET /projects/{pid}/preview` — mirrors `UploadPreview` in `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UploadPreview {
    pub title: String,
    pub fmt: String,
    pub chapter_count: u32,
    pub total_word_count: u64,
    pub source_lang: Option<String>,
    #[serde(default)]
    pub chapters: Vec<PreviewChapter>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct PreviewChapter {
    pub index: u32,
    pub title: String,
    pub word_count: u64,
}

/// `GET /projects/{pid}/chapters[]` — mirrors `ChapterSummary` in `schemas.py`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ChapterSummary {
    pub index: u32,
    #[serde(default)]
    pub title: String,
    pub title_translated: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub word_count: u64,
    #[serde(default)]
    pub target_word_count: u64,
    #[serde(default)]
    pub review_issue_count: u32,
    #[serde(default)]
    pub review_status: String,
}

/// `GET /projects/{pid}/stats` — mirrors `ProjectStats` in `schemas.py`.
/// Usage/timing are free-form ledgers; the UI reads known keys defensively.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
pub struct ProjectStats {
    #[serde(default)]
    pub usage: serde_json::Value,
    #[serde(default)]
    pub timing: serde_json::Value,
    pub live: Option<serde_json::Value>,
}

/// `GET /projects/{pid}/workflow` — mirrors `WorkflowOut` in `schemas.py`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct WorkflowOut {
    pub source: String,
    pub kind: String,
    pub status: String,
    pub run_id: Option<String>,
    pub review_id: Option<String>,
    #[serde(default)]
    pub stages: Vec<WorkflowStage>,
    pub progress: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct WorkflowStage {
    pub id: String,
    pub label: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// `TermOut` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GlossaryTerm {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub reading: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub gender: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub first_chapter: Option<i64>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub status: String,
}

/// `TermIn` — mirrors `schemas.py` (add/update body).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TermIn {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub reading: String,
    #[serde(default = "default_term_type")]
    pub r#type: String,
    #[serde(default)]
    pub gender: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub note: String,
}

fn default_term_type() -> String {
    "term".into()
}

/// `ConflictOut` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TermConflict {
    pub id: i64,
    pub source: String,
    pub existing_target: Option<String>,
    pub proposed_target: Option<String>,
    pub chapter: Option<i64>,
    pub note: Option<String>,
}

/// `SegmentOut` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SegmentOut {
    pub index: u32,
    pub source: String,
    pub target: Option<String>,
    pub target_before_polish: Option<String>,
    pub anchor: Option<String>,
    #[serde(default)]
    pub kind: String,
}

/// `ChapterSegments` — mirrors `schemas.py` (`GET …/review/{ci}`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChapterSegments {
    pub index: u32,
    #[serde(default)]
    pub title: String,
    pub title_translated: Option<String>,
    #[serde(default)]
    pub segments: Vec<SegmentOut>,
    #[serde(default)]
    pub review_issues: Vec<serde_json::Value>,
}

/// `SegmentRevision` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SegmentRevision {
    pub id: String,
    pub kind: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// `ReviewRun` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReviewRun {
    pub id: String,
    pub review_id: String,
    pub status: String,
    pub created_at: Option<String>,
    #[serde(default)]
    pub issues: Vec<serde_json::Value>,
    #[serde(default)]
    pub changes: Vec<serde_json::Value>,
    #[serde(default)]
    pub autofix: serde_json::Value,
    #[serde(default)]
    pub summary: serde_json::Value,
    #[serde(default)]
    pub result: serde_json::Value,
    #[serde(default)]
    pub items: Vec<ReviewItem>,
}

/// `ReviewItem` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReviewItem {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub suggestion: String,
    pub status: String,
    pub location: Option<ReviewLocation>,
    #[serde(default)]
    pub evidence: Vec<ReviewLocation>,
}

/// `ReviewLocation` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReviewLocation {
    pub chapter: i64,
    pub text_index: i64,
    pub segment_index: i64,
    #[serde(default)]
    pub chapter_title: String,
    #[serde(default)]
    pub source: String,
    pub current_target: Option<String>,
}

/// `AnalysisOut` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnalysisOut {
    #[serde(default)]
    pub analysis: serde_json::Value,
    #[serde(default)]
    pub chapter_digests: Vec<ChapterDigest>,
}

/// `ChapterDigest` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChapterDigest {
    pub index: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub digest: String,
}

/// Subtitle cue — mirrors the `get_subtitles` row shape.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SubtitleCue {
    pub id: String,
    pub index: u32,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: String,
    #[serde(default)]
    pub source: String,
    pub target: Option<String>,
    #[serde(default)]
    pub status: String,
}

/// `GET …/subtitles` — mirrors the response shape.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SubtitleList {
    #[serde(default)]
    pub cues: Vec<SubtitleCue>,
    #[serde(default)]
    pub completed: u32,
    #[serde(default)]
    pub total: u32,
}

/// `ExportRequest` — mirrors `schemas.py`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ExportRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    pub bilingual: bool,
    pub order: String,
    pub about_page: bool,
    pub preserve_source_style: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub punctuation_normalize: Option<bool>,
    pub pdf_engine: String,
}

impl ExportRequest {
    pub fn new() -> Self {
        Self {
            format: None,
            bilingual: false,
            order: "target_first".into(),
            about_page: true,
            preserve_source_style: false,
            punctuation_normalize: None,
            pdf_engine: "weasyprint".into(),
        }
    }
}

/// `ExportOut` — mirrors `schemas.py`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ExportOut {
    #[serde(default)]
    pub options: serde_json::Value,
    pub error: Option<String>,
    pub id: i64,
    pub project_id: String,
    pub format: String,
    pub status: String,
    pub path: Option<String>,
    pub size: Option<i64>,
    pub created_at: Option<String>,
}

/// `NativeExportPlan` — mirrors `desktop_export.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NativeExportPlan {
    pub filename: String,
    pub extension: String,
    pub label: String,
    #[serde(default)]
    pub protected_roots: Vec<String>,
}

/// `EventOut` — mirrors `schemas.py`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct EventOut {
    pub id: i64,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub created_at: Option<String>,
}

/// `ProjectConfigOut` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProjectConfigOut {
    pub yaml: String,
    #[serde(default)]
    pub effective: serde_json::Value,
    #[serde(default)]
    pub routes: Vec<serde_json::Value>,
    pub editable: bool,
    #[serde(default)]
    pub registered_models: serde_json::Value,
}

/// `GlobalConfigOut` — mirrors `schemas.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GlobalConfigOut {
    pub yaml: String,
    #[serde(default)]
    pub effective: serde_json::Value,
    pub default_template: String,
    pub revision: i64,
}
