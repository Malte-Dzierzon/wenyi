//! One-book translation parity against the pinned oracle.
//!
//! Drives `Pipeline::prepare` + `Pipeline::translate` over
//! `tests/golden/corpus/two_chapters.txt` with a scripted fake handler
//! mirroring `packages/core/tests/fake_llm.py::routing_handler`, then
//! compares against `translate_txt.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use wenyi_core::config;
use wenyi_core::llm::compat::Message;
use wenyi_core::llm::fake::{FakeClient, FakeHandler};
use wenyi_core::model::ProjectId;
use wenyi_core::pipeline::{Pipeline, Progress};
use wenyi_core::{glossary, store};

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("corpus")
        .join(name)
}

fn golden() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("snapshots")
        .join("translate_txt.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn count_numbered(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            line.starts_with('[') && line.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
        })
        .count()
}

/// Mirror of the oracle `routing_handler` (fake_llm.py).
fn handler() -> FakeHandler {
    Box::new(|messages: &[Message], _tier: &str, json_mode: bool| {
        let system = messages
            .first()
            .map(|message| message.content.as_str())
            .unwrap_or("");
        let user = messages
            .last()
            .map(|message| message.content.as_str())
            .unwrap_or("");
        if system.contains("language detector") {
            return r#"{"language": "ja"}"#.to_string();
        }
        if system.contains("pre-translation analyst") {
            return serde_json::json!({
                "genre": "校园",
                "tone": "冷峻",
                "style_guide": "克制",
                "characters": [{"source": "綾小路", "target": "绫小路", "gender": "male"}],
                "terms": [],
            })
            .to_string();
        }
        if system.contains("chapter title translator") {
            let n = count_numbered(user);
            return serde_json::json!({
                "titles": (0..n).map(|i| format!("标题{i}")).collect::<Vec<_>>(),
            })
            .to_string();
        }
        if system.contains("literary translator") {
            if user.contains("Polish the translations from your previous JSON response") {
                let mut n = None;
                for message in messages[..messages.len().saturating_sub(1)].iter().rev() {
                    if message.role == "assistant" {
                        let payload: serde_json::Value =
                            serde_json::from_str(&message.content).unwrap_or_default();
                        if let Some(items) = payload.get("translations").and_then(|v| v.as_array())
                        {
                            n = Some(items.len());
                        }
                        break;
                    }
                }
                let n = n.unwrap_or_else(|| {
                    user.split("exactly ")
                        .nth(1)
                        .and_then(|rest| rest.split_whitespace().next())
                        .and_then(|num| num.parse().ok())
                        .unwrap_or(0)
                });
                return serde_json::json!({
                    "polished": (0..n).map(|i| format!("润{i}")).collect::<Vec<_>>(),
                })
                .to_string();
            }
            let n = count_numbered(user);
            return serde_json::json!({
                "translations": (0..n).map(|i| format!("译{i}")).collect::<Vec<_>>(),
            })
            .to_string();
        }
        if system.contains("prose editor") {
            let n = count_numbered(user);
            return serde_json::json!({
                "polished": (0..n).map(|i| format!("润{i}")).collect::<Vec<_>>(),
            })
            .to_string();
        }
        if system.contains("translation reviewer") {
            let n = count_numbered(user);
            return serde_json::json!({
                "issues": [],
                "reviewed_segments": n,
                "complete": true,
            })
            .to_string();
        }
        if system.contains("terminology") && system.contains("extractor") {
            return serde_json::json!({
                "terms": [{"source": "堀北", "target": "堀北", "type": "person", "gender": "female"}],
            })
            .to_string();
        }
        if system.contains("chapter digest writer") {
            return "本章梗概：人物登场，情节推进。".to_string();
        }
        if system.contains("whole-book synopsis writer") {
            return "全书概览：主线与人物关系，整体基调。".to_string();
        }
        if json_mode {
            "{}".to_string()
        } else {
            String::new()
        }
    })
}

fn fake_config() -> config::Config {
    config::load(
        r#"
language: {source: ja, target: zh}
llm:
  preset: fake
  models:
    default_strong: {provider: default, model: p}
    default_cheap: {provider: default, model: f}
segment: {max_tokens_per_batch: 1800}
pipeline: {review: false, review_autofix: false, polish: true, style_observation: false}
"#,
    )
    .unwrap()
}

fn temp_db(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("wenyi-pipeline-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Copy the corpus next to the DB (prepare takes an input path).
    let src = dir.join("novel.txt");
    std::fs::copy(corpus("two_chapters.txt"), &src).unwrap();
    store::db_path(&dir)
}

/// One chapter: ordinal, title, (ordinal, target, has_baseline) segments.
type ChapterOutcome = (usize, String, Vec<(usize, Option<String>, bool)>);
/// Chapters, usage totals, events, model call count.
type RunOutcome = (
    Vec<ChapterOutcome>,
    serde_json::Value,
    Vec<serde_json::Value>,
    usize,
);

fn run_pipeline() -> RunOutcome {
    let db = temp_db("translate");
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("test-project").unwrap();
    store::create_project(&conn, &project, "Test").unwrap();
    let mut fake = FakeClient::new(Some(handler()), None).unwrap();
    let mut pipeline = Pipeline::new(fake_config(), &mut fake);
    let input = db.parent().unwrap().join("novel.txt");
    let (book_id, _synopsis) = pipeline
        .prepare(&conn, &project, &input, &mut Progress::none())
        .unwrap();
    let mut events = Vec::new();
    pipeline
        .translate(
            &conn,
            &project,
            &book_id,
            "全书概览：主线与人物关系，整体基调。",
            None,
            &mut Progress::none(),
            &mut events,
        )
        .unwrap();
    let chapters = store::list_chapters(&conn, &book_id, 0, i64::MAX).unwrap();
    let mut out = Vec::new();
    for chapter in &chapters {
        let segments = store::list_segments(&conn, &chapter.id, 0, i64::MAX).unwrap();
        out.push((
            chapter.ordinal as usize,
            chapter.title.clone(),
            segments
                .iter()
                .map(|segment| {
                    (
                        segment.ordinal as usize,
                        segment.target.clone(),
                        segment.target_before_polish.is_some(),
                    )
                })
                .collect(),
        ));
    }
    let usage = pipeline.usage.summary();
    let usage_totals = usage.get("totals").cloned().unwrap_or_default();
    let calls = pipeline.client.call_count();
    (out, usage_totals, events, calls)
}

#[test]
fn translate_txt_matches_oracle() {
    let (chapters, usage_totals, events, calls) = run_pipeline();
    let want = golden();
    assert_eq!(chapters.len(), 2, "two chapters");
    for (got, want_chapter) in chapters.iter().zip(want["chapters"].as_array().unwrap()) {
        assert_eq!(got.0, want_chapter["index"].as_u64().unwrap() as usize);
        assert_eq!(got.1, want_chapter["title"].as_str().unwrap());
        let want_segments = want_chapter["segments"].as_array().unwrap();
        assert_eq!(got.2.len(), want_segments.len());
        for (got_seg, want_seg) in got.2.iter().zip(want_segments) {
            assert_eq!(got_seg.0, want_seg["index"].as_u64().unwrap() as usize);
            assert_eq!(
                got_seg.1.as_deref(),
                want_seg["target"].as_str(),
                "chapter {} segment {} target",
                got.0,
                got_seg.0
            );
            assert_eq!(
                got_seg.2,
                want_seg["has_polish_baseline"].as_bool().unwrap(),
                "chapter {} segment {} baseline",
                got.0,
                got_seg.0
            );
        }
    }
    let translated: usize = chapters
        .iter()
        .flat_map(|(_, _, segments)| segments)
        .filter(|(_, target, _)| target.is_some())
        .count();
    assert_eq!(
        translated,
        want["translated_segments"].as_u64().unwrap() as usize
    );
    // Usage totals: same shape as the oracle MeteredFakeClient ledger.
    for key in [
        "calls",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "cache_hit_tokens",
        "cache_miss_tokens",
        "completion_ms",
        "timed_calls",
        "timed_completion_tokens",
    ] {
        assert_eq!(
            usage_totals.get(key),
            want["usage_totals"].get(key),
            "usage {key} (calls={calls})"
        );
    }
    assert!(
        (usage_totals["cache_hit_rate"].as_f64().unwrap_or(-1.0)
            - want["usage_totals"]["cache_hit_rate"].as_f64().unwrap())
        .abs()
            < f64::EPSILON
    );
    // Run-boundary events.
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|event| event.get("event").and_then(|v| v.as_str()))
        .collect();
    assert!(kinds.contains(&"translate_run_started"));
    assert!(kinds.contains(&"translate_run_finished"));
    let started = events
        .iter()
        .find(|event| event.get("event").and_then(|v| v.as_str()) == Some("translate_run_started"))
        .unwrap();
    assert_eq!(
        started.clone(),
        want["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["event"] == "translate_run_started")
            .unwrap()
            .clone()
    );
    // Fake call count: 2 digests + 1 analysis + 1 synopsis + 2 batches ×
    // (translate + polish-continue) + 2 batch extracts + 2 chapter extracts
    // + 1 titles = 13 model calls, matching oracle usage totals.
    let _ = &BTreeMap::<String, String>::new();
    assert_eq!(calls, 13, "model call count");
}

#[test]
fn resume_skips_completed_batches_without_retranslate() {
    let db = temp_db("resume");
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("test-resume").unwrap();
    store::create_project(&conn, &project, "Test").unwrap();
    // First run: only chapter 0 (partial run, then resume).
    let mut fake = FakeClient::new(Some(handler()), None).unwrap();
    let mut pipeline = Pipeline::new(fake_config(), &mut fake);
    let input = db.parent().unwrap().join("novel.txt");
    let (book_id, synopsis) = pipeline
        .prepare(&conn, &project, &input, &mut Progress::none())
        .unwrap();
    let mut events = Vec::new();
    pipeline
        .translate(
            &conn,
            &project,
            &book_id,
            &synopsis,
            Some(0),
            &mut Progress::none(),
            &mut events,
        )
        .unwrap();
    let first_calls = pipeline.client.call_count();
    drop(pipeline);
    drop(fake);
    // Resume: chapter 0 done, chapter 1 pending. Chapter 0 is not in the
    // target list; a forced re-run of chapter 0 must skip its batch.
    let mut fake = FakeClient::new(Some(handler()), None).unwrap();
    let mut pipeline = Pipeline::new(fake_config(), &mut fake);
    let (book_id2, synopsis2) = pipeline
        .prepare(&conn, &project, &input, &mut Progress::none())
        .unwrap();
    assert_eq!(book_id, book_id2);
    assert_eq!(pipeline.client.call_count(), 0, "digests/synopsis cached");
    let mut events2 = Vec::new();
    pipeline
        .translate(
            &conn,
            &project,
            &book_id2,
            &synopsis2,
            Some(0),
            &mut Progress::none(),
            &mut events2,
        )
        .unwrap();
    // Forced rerun of a completed chapter: the batch is skipped (no
    // translate/polish), but the oracle's unconditional chapter-wide
    // extraction fallback still runs one glossary call.
    assert_eq!(
        pipeline.client.call_count(),
        1,
        "only chapter-wide extraction reruns"
    );
    let skipped = events2
        .iter()
        .filter(|event| event.get("event").and_then(|v| v.as_str()) == Some("batch_skipped"))
        .count();
    assert_eq!(skipped, 1, "completed batch skipped");
    drop(pipeline);
    drop(fake);
    // Full resume finishes chapter 1.
    let mut fake = FakeClient::new(Some(handler()), None).unwrap();
    let mut pipeline = Pipeline::new(fake_config(), &mut fake);
    let mut events3 = Vec::new();
    pipeline
        .translate(
            &conn,
            &project,
            &book_id,
            &synopsis,
            None,
            &mut Progress::none(),
            &mut events3,
        )
        .unwrap();
    assert!(
        pipeline.client.call_count() > 0,
        "pending chapter translates on resume"
    );
    assert!(first_calls > 0, "partial run used the model");
}

#[test]
fn glossary_seeds_and_extracts() {
    let db = temp_db("glossary");
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("test-glossary").unwrap();
    store::create_project(&conn, &project, "Test").unwrap();
    let mut fake = FakeClient::new(Some(handler()), None).unwrap();
    let mut pipeline = Pipeline::new(fake_config(), &mut fake);
    let input = db.parent().unwrap().join("novel.txt");
    let (book_id, synopsis) = pipeline
        .prepare(&conn, &project, &input, &mut Progress::none())
        .unwrap();
    let seeded = glossary::all_terms(&conn, &project, "ja", "zh").unwrap();
    assert_eq!(seeded.len(), 1, "analyzer seeds one character");
    assert_eq!(seeded[0].source, "綾小路");
    assert_eq!(seeded[0].target, "绫小路");
    let mut events = Vec::new();
    pipeline
        .translate(
            &conn,
            &project,
            &book_id,
            &synopsis,
            None,
            &mut Progress::none(),
            &mut events,
        )
        .unwrap();
    let terms = glossary::all_terms(&conn, &project, "ja", "zh").unwrap();
    let sources: Vec<&str> = terms.iter().map(|term| term.source.as_str()).collect();
    assert!(sources.contains(&"綾小路"), "seeded term kept: {sources:?}");
    assert!(
        sources.contains(&"堀北"),
        "extracted term stored: {sources:?}"
    );
    // Titles translated via the title agent.
    let chapters = store::list_chapters(&conn, &book_id, 0, i64::MAX).unwrap();
    for chapter in &chapters {
        assert!(
            chapter
                .title_translated
                .as_ref()
                .is_some_and(|t| t.starts_with("标题")),
            "title translated for {:?}",
            chapter.title
        );
    }
}

#[test]
fn second_import_becomes_next_book_and_same_hash_resumes() {
    let db = temp_db("series");
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("test-series").unwrap();
    store::create_project(&conn, &project, "Test").unwrap();
    let mut fake = FakeClient::new(Some(handler()), None).unwrap();
    let mut pipeline = Pipeline::new(fake_config(), &mut fake);
    let input = db.parent().unwrap().join("novel.txt");
    let (book_a, _) = pipeline
        .prepare(&conn, &project, &input, &mut Progress::none())
        .unwrap();
    // Same content again: resume, no duplicate book.
    let (book_a2, _) = pipeline
        .prepare(&conn, &project, &input, &mut Progress::none())
        .unwrap();
    assert_eq!(book_a, book_a2);
    assert_eq!(store::list_books(&conn, &project).unwrap().len(), 1);
    // Distinct content: second book at ordinal 1.
    let other = db.parent().unwrap().join("other.txt");
    std::fs::write(&other, "# Other\n\nDifferent words here.\n").unwrap();
    let (book_b, _) = pipeline
        .prepare(&conn, &project, &other, &mut Progress::none())
        .unwrap();
    assert_ne!(book_a, book_b);
    let books = store::list_books(&conn, &project).unwrap();
    assert_eq!(books.len(), 2);
    assert_eq!(books[1].id, book_b);
    assert_eq!(books[1].ordinal, 1);
}
