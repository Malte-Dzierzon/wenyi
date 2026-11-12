//! Review domain parity: keys, digests, session transitions.

use std::collections::BTreeMap;
use wenyi_core::review_models::{
    content_digest, review_candidate_id, review_issue_key, review_overlay_digest, ReviewPolicy,
    ReviewRoundResult, ReviewSessionState, SegmentRef,
};

fn issue(
    chapter: i64,
    index: i64,
    kind: &str,
    detail: &str,
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({"chapter": chapter, "index": index, "type": kind, "detail": detail})
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn identities_match_oracle() {
    assert_eq!(
        review_candidate_id(1, 0, 2, Some(3)),
        "r3-ch1-base0-candidate2"
    );
    assert_eq!(review_candidate_id(1, 0, 2, None), "ch1-base0-candidate2");
    let key = review_issue_key(&issue(1, 2, "Mistranslation ", "Wrong  name"));
    assert_eq!(key, "review-issue-a52a3aaf18edf768286e");
    let chapters = vec![(1, vec!["B".to_string(), String::new()])];
    let mut overrides = BTreeMap::new();
    overrides.insert((1, 1), "X".to_string());
    assert_eq!(
        review_overlay_digest(&chapters, &overrides),
        "48a4fdb7f6297f91e67e44180be0f8af926cd16cdc695ef39305c95b57419405"
    );
    let formal = vec![(
        1,
        vec![
            (
                0,
                String::new(),
                "text".to_string(),
                "A".to_string(),
                "B".to_string(),
            ),
            (
                1,
                "a".to_string(),
                "text".to_string(),
                "C".to_string(),
                String::new(),
            ),
        ],
    )];
    assert_eq!(
        content_digest(&formal),
        "920450aa99706d77f14f52eed07ce8d13f2c23522247747282c006b4dc4a66e6"
    );
    let segment = SegmentRef {
        global_ordinal: 7,
        chapter: 1,
        text_index: 2,
        segment_index: 3,
        source: "S".to_string(),
        target: "T".to_string(),
        baseline_target: "B".to_string(),
        target_origin: "shadow_override".to_string(),
        chapter_title: "C".to_string(),
    };
    assert_eq!(segment.reference(), "ch1:text2:seg3");
    assert_eq!(segment.compact()["baseline_target"], "B");
}

#[test]
fn policy_round_limits_match_oracle() {
    let policy = ReviewPolicy {
        fix_loop: true,
        max_fix_rounds: 2,
        clean_confirmations: 2,
    };
    assert_eq!(policy.required_clean(), 2);
    assert_eq!(policy.max_review_rounds(), 6);
    let off = ReviewPolicy {
        fix_loop: false,
        max_fix_rounds: 2,
        clean_confirmations: 2,
    };
    assert_eq!(off.required_clean(), 1);
    assert_eq!(off.max_review_rounds(), 1);
}

#[test]
fn after_scan_drives_fix_then_clean_confirmed() {
    let policy = ReviewPolicy {
        fix_loop: true,
        max_fix_rounds: 2,
        clean_confirmations: 1,
    };
    let mut state = ReviewSessionState::default();
    let mut round = ReviewRoundResult::default();
    round.issues.push(issue(0, 0, "missing", "gap"));
    let decision = state.after_scan(&round, 1, "digest-a", policy);
    assert_eq!(decision.action, "fix");
    assert_eq!(decision.summary["issue_count"], 1);
    let clean = ReviewRoundResult::default();
    let decision = state.after_scan(&clean, 2, "digest-b", policy);
    assert_eq!(decision.action, "stop");
    assert_eq!(state.termination, "clean_confirmed");
}

#[test]
fn prepare_and_after_fix_accept_one_round_then_detect_cycle() {
    let policy = ReviewPolicy {
        fix_loop: true,
        max_fix_rounds: 4,
        clean_confirmations: 1,
    };
    let _ = policy;
    let mut state = ReviewSessionState::default();
    let chapters = vec![(0, vec!["old".to_string()])];
    let mut current = BTreeMap::new();
    current.insert((0, 0), "old".to_string());
    let mut latest = ReviewRoundResult::default();
    let mut first = issue(0, 0, "missing", "gap");
    first.insert("issue_id".to_string(), serde_json::json!("r1-review-00001"));
    first.insert("issue_key".to_string(), serde_json::json!("k1"));
    latest.issues.push(first);
    let before = wenyi_core::review_models::text_hash("old");
    let patch = serde_json::json!({
        "patch_id": "patch-r1-x", "chapter": 0, "index": 0,
        "issue_ids": ["r1-review-00001"], "before_hash": before, "after": "new",
    })
    .as_object()
    .unwrap()
    .clone();
    let mut summary = serde_json::Map::new();
    let plan = state
        .prepare_fix(
            &latest,
            vec![patch],
            Vec::new(),
            &current,
            &chapters,
            1,
            &mut summary,
        )
        .unwrap();
    assert_eq!(plan.applicable.len(), 1);
    assert!(state.after_fix(&plan, "digest-a", &mut summary));
    assert_eq!(state.fix_rounds, 1);
    // Same candidate again: cycle detected.
    let patch2 = serde_json::json!({
        "patch_id": "patch-r2-x", "chapter": 0, "index": 0,
        "issue_ids": ["r1-review-00001"], "before_hash": before, "after": "new",
    })
    .as_object()
    .unwrap()
    .clone();
    // Note: variable `before` moved; recompute is unnecessary — reuse fails
    // to compile, so rebuild the plan from a fresh patch map inline.
    let _ = patch2;
    let plan2 = state
        .prepare_fix(
            &latest,
            vec![serde_json::json!({
                "patch_id": "patch-r2-x", "chapter": 0, "index": 0,
                "issue_ids": ["r1-review-00001"],
                "before_hash": wenyi_core::review_models::text_hash("old"),
                "after": "new",
            })
            .as_object()
            .unwrap()
            .clone()],
            Vec::new(),
            &current,
            &chapters,
            2,
            &mut summary,
        )
        .unwrap();
    assert!(!state.after_fix(&plan2, "digest-b", &mut summary));
    assert_eq!(state.termination, "cycle_detected");
}

#[test]
fn evidence_tools_answer_bounded_queries() {
    use wenyi_core::glossary::GlossaryTerm;
    use wenyi_core::review_evidence::{BookEvidenceIndex, EvidenceChapter};

    let chapters = vec![EvidenceChapter {
        index: 0,
        title: "C".to_string(),
        digest: "D".to_string(),
        segments: vec![
            (
                0,
                0,
                "The old Wolf prowls.".to_string(),
                "老狼潜行。".to_string(),
            ),
            (1, 1, "Moon rises.".to_string(), "月升。".to_string()),
        ],
    }];
    let mut analysis = serde_json::Map::new();
    analysis.insert("style_guide".to_string(), serde_json::json!("Be terse."));
    analysis.insert(
        "book_synopsis".to_string(),
        serde_json::json!("A wolf tale."),
    );
    let mut wolf = GlossaryTerm::new("Wolf", "狼");
    wolf.term_type = "person".to_string();
    let mut index = BookEvidenceIndex::new(&chapters, vec![wolf], analysis, "en", &BTreeMap::new());
    // Glossary term lookup.
    let found = index.glossary_term(
        &serde_json::json!({"term": "Wolf"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(found["ok"], true);
    assert!(found["term"]["ref"]
        .as_str()
        .unwrap()
        .starts_with("glossary:"));
    assert!(found["term"].get("reading").is_none());
    let missing = index.glossary_term(
        &serde_json::json!({"term": "Nope"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(missing["error"], "term_not_found");
    // Occurrences with selectors.
    let occ = index.term_occurrences(
        &serde_json::json!({"term": "Wolf", "selectors": ["first"]})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(occ["ok"], true);
    assert_eq!(occ["total_matches"], 1);
    assert_eq!(occ["selected"].as_array().unwrap().len(), 1);
    let bad = index.term_occurrences(
        &serde_json::json!({"term": "Wolf", "selectors": [9]})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(bad["invalid_selectors"].as_array().unwrap().len(), 1);
    // Segment context across bounds.
    let ctx = index.segment_context(
        &serde_json::json!({"chapter": 0, "index": 1, "before": 5, "after": 5})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(ctx["ok"], true);
    assert_eq!(ctx["segments"].as_array().unwrap().len(), 2);
    // Book context sections.
    let guide = index.book_context(
        &serde_json::json!({"section": "style_guide"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(guide["ref"], "book:style_guide");
    let digest = index.book_context(
        &serde_json::json!({"section": "chapter_digest", "chapter": 0})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(digest["ref"], "book:chapter_digest:ch0");
    // Execute wraps with request id + tool and collects refs.
    let result = index.execute(
        &serde_json::json!({
            "request_id": "r1", "tool": "glossary_term", "arguments": {"term": "Wolf"}
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    assert_eq!(result["request_id"], "r1");
    let refs = BookEvidenceIndex::evidence_refs(&serde_json::Value::Object(result));
    assert!(refs.iter().any(|r| r.starts_with("glossary:")), "{refs:?}");
    let unknown = index.execute(
        &serde_json::json!({"request_id": "r2", "tool": "nope", "arguments": {}})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(unknown["error"], "unknown_tool");
}

#[test]
fn conflicts_group_arbitrate_and_rewrite() {
    use wenyi_core::glossary::GlossaryTerm;
    use wenyi_core::review_conflicts::{
        apply_review_arbitrations, build_conflict_groups, normalize_review_issues,
    };
    use wenyi_core::review_evidence::{BookEvidenceIndex, EvidenceChapter};

    let chapters = vec![
        EvidenceChapter {
            index: 0,
            title: String::new(),
            digest: String::new(),
            segments: vec![(0, 0, "Wolf".to_string(), "狼".to_string())],
        },
        EvidenceChapter {
            index: 1,
            title: String::new(),
            digest: String::new(),
            segments: vec![(0, 0, "Wolf".to_string(), "狼".to_string())],
        },
    ];
    let index = BookEvidenceIndex::new(
        &chapters,
        vec![GlossaryTerm::new("Wolf", "狼")],
        serde_json::Map::new(),
        "en",
        &BTreeMap::new(),
    );
    let raw = |chapter: i64, chunk: &str, proposed: &str| {
        let issue = serde_json::json!({
            "chapter": chapter, "index": 0, "_chunk_id": chunk, "type": "terminology",
            "detail": "d", "suggestion": "s",
            "consistency": {"kind": "term", "subject_source": "Wolf", "proposed_value": proposed},
        })
        .as_object()
        .unwrap()
        .clone();
        issue
    };
    let normalized = normalize_review_issues(&[raw(0, "a", "苍狼"), raw(1, "b", "灰狼")], &index);
    assert_eq!(normalized[0]["consistency"]["key"], "term:glossary:Wolf");
    assert!(normalized[0]["issue_id"]
        .as_str()
        .unwrap()
        .starts_with("review-"));
    let groups = build_conflict_groups(&normalized);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["conflict_id"], "review-conflict-0001");
    let arbitration = serde_json::json!({
        "conflict_id": "review-conflict-0001", "status": "suggested",
        "recommended_value": "苍狼", "reason": "archaic fits",
        "supported_issue_ids": [normalized[0]["issue_id"]],
        "rejected_issue_ids": [normalized[1]["issue_id"]],
    })
    .as_object()
    .unwrap()
    .clone();
    let (final_issues, superseded) = apply_review_arbitrations(&normalized, &[arbitration]);
    assert_eq!(final_issues.len(), 2);
    assert_eq!(superseded.len(), 1);
    assert!(final_issues[1]["detail"].as_str().unwrap().contains("苍狼"));
    assert_eq!(final_issues[1]["consistency"]["proposed_value"], "苍狼");
}
