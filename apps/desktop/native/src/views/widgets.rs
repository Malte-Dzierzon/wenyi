//! Shared stat/chart widgets for the Translation Overview page.
//!
//! Used ONLY by `views/progress.rs`. Mirrors the Accounting block of
//! `features/progress/ProgressPage.tsx` + `Accounting.tsx`:
//! metric cards (cumulative tokens / model requests / cache hit rate /
//! recorded time), usage breakdown (stacked token bars per stage/model/
//! provider), and run history (duration bars with status).
//!
//! Charts use the `gpui-component` chart module (`LineChart::new`, kit
//! verdict: ACCEPTED — same workspace dependency, `gpui-kit = "0.8"` →
//! `gpui_kit::component::chart`) with theme-aware colors from
//! [`theme::Palette`]. Empty states render text ("No usage yet"
//! / "No runs yet") instead of empty charts.
//!
//! # Metric → JSON path map (all paths read defensively; missing → "—")
//!
//! | Card              | JSON path (stats response)                                  |
//! |-------------------|-------------------------------------------------------------|
//! | Cumulative tokens | `usage.totals.total_tokens` (u64)                         |
//! | Model requests    | `usage.totals.calls` (u64)                                |
//! | Cache hit rate    | `usage.totals.cache_hit_tokens` / `.cache_miss_tokens`;  |
//! |                   | guarded exactly like `cacheRate` in `accountingData.ts`:  |
//! |                   | undefined when `hit + miss == 0` or `hit + miss > prompt` |
//! | Recorded time     | `timing.total_seconds` (f64)                                |
//!
//! | Chart             | JSON paths                                                |
//! |-------------------|-------------------------------------------------------------|
//! | Usage breakdown   | `usage.{by_stage,by_model,by_provider}[id].{calls,       |
//! |                   | prompt_tokens,completion_tokens,total_tokens,             |
//! |                   | cache_hit_tokens,cache_miss_tokens}`, labels from        |
//! |                   | `usage.labels[id]` (stage ids fall back to readable      |
//! |                   | operation names like the source). Token segments mirror  |
//! |                   | `tokenParts` in `accountingData.ts`.                     |
//! | Run history       | `timing.runs[] .{id,operation,status,started_at,         |
//! |                   | elapsed_seconds}` newest-first by `started_at` (mirrors  |
//! |                   | `timingRuns` in `accountingData.ts`).                    |
//!
//! Live tick: the web `useLiveTiming` adds elapsed seconds for `running`
//! runs on a 1s client timer. The native view re-renders on poll
//! (`progressInterval`: 2.5s connected / 5s report while busy), so
//! `timing.total_seconds` from the ledger is already fresh — no local clock.

use gpui_kit::component::chart::{LineChart, PieChart};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::*;

use crate::theme;

/// One usage-breakdown row: display label + token counters.
#[derive(Debug, Clone)]
pub struct UsageRow {
    pub label: String,
    pub total: f64,
    pub prompt: f64,
    pub completion: f64,
    pub cache_hit: f64,
    pub cache_miss: f64,
    pub calls: u64,
}

/// One run-history entry.
#[derive(Debug, Clone)]
pub struct RunRow {
    pub id: String,
    pub operation: String,
    pub status: String,
    pub started_at: String,
    pub seconds: f64,
}

/// Grouping key for the usage breakdown, mirroring `UsageGroup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageGroup {
    ByStage,
    ByModel,
    ByProvider,
}

impl UsageGroup {
    pub const ALL: [UsageGroup; 3] = [UsageGroup::ByStage, UsageGroup::ByModel, UsageGroup::ByProvider];

    pub fn key(self) -> &'static str {
        match self {
            UsageGroup::ByStage => "by_stage",
            UsageGroup::ByModel => "by_model",
            UsageGroup::ByProvider => "by_provider",
        }
    }

    pub fn tab_label(self) -> &'static str {
        match self {
            UsageGroup::ByStage => "By stage",
            UsageGroup::ByModel => "By model",
            UsageGroup::ByProvider => "By provider",
        }
    }
}

// ---------------------------------------------------------------------------
// JSON accessors (defensive; mirrors accountingData.ts record/amount)
// ---------------------------------------------------------------------------

fn num(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64().filter(|v| v.is_finite() && *v >= 0.0),
        _ => None,
    }
}

fn int(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(n) => {
            if n.is_u64() {
                n.as_u64()
            } else if n.is_i64() {
                u64::try_from(n.as_i64()?).ok()
            } else {
                n.as_f64().filter(|v| v.is_finite() && *v >= 0.0).map(|v| v as u64)
            }
        }
        _ => None,
    }
}

fn obj<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    value.get(key).filter(|v| v.is_object())
}

fn str_field(value: &serde_json::Value, key: &str) -> String {
    value.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

/// `cacheRate` from accountingData.ts: hit share of cache traffic, undefined
/// unless there is cache traffic that fits inside the prompt tokens.
pub fn cache_rate(hit: Option<f64>, miss: Option<f64>, input: Option<f64>) -> Option<f64> {
    let (hit, miss) = (hit?, miss?);
    if hit + miss == 0.0 {
        return None;
    }
    if let Some(input) = input {
        if hit + miss > input {
            return None;
        }
    }
    Some(hit / (hit + miss))
}

/// `formatDuration` from accountingData.ts: seconds → "4.1 s" / "6m 6s" /
/// "22m 13s" / "1h 2m 3s".
pub fn format_duration(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds else { return "—".into() };
    if seconds < 60.0 {
        let rounded = (seconds * 10.0).round() / 10.0;
        if rounded == rounded.trunc() {
            return format!("{} s", rounded as i64);
        }
        return format!("{rounded} s");
    }
    let total = seconds.floor() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {secs}s")
    } else {
        format!("{minutes}m {secs}s")
    }
}

/// Format an integer with thousands separators ("496,861"), mirroring the
/// locale-aware `toLocaleString` in Accounting.tsx.
pub fn format_count(value: Option<u64>) -> String {
    let Some(mut value) = value else { return "—".into() };
    if value == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while value >= 1000 {
        out.push(format!("{:03}", value % 1000));
        value /= 1000;
    }
    out.push(value.to_string());
    out.iter().rev().cloned().collect::<Vec<_>>().join(",")
}

// ---------------------------------------------------------------------------
// Metric extraction (tested pure logic)
// ---------------------------------------------------------------------------

/// Four headline metrics for the stats card. `running` is accepted for API
/// parity with the source `running` prop; the native poll already refreshes
/// the ledger so no local tick is applied.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageMetrics {
    /// `usage.totals.total_tokens`
    pub total_tokens: Option<u64>,
    /// `usage.totals.calls`
    pub calls: Option<u64>,
    /// `usage.totals.cache_hit_tokens/miss_tokens` (+ prompt guard)
    pub cache_rate: Option<f64>,
    /// `timing.total_seconds`
    pub total_seconds: Option<f64>,
    /// `total_tokens / total_seconds` (None when either is missing or zero)
    pub tokens_per_second: Option<f64>,
}

impl UsageMetrics {
    pub fn from_stats(stats: &crate::client::ProjectStats, _running: bool) -> Self {
        let totals = obj(&stats.usage, "totals");
        let missing = serde_json::Value::Null;
        let total_tokens = totals.and_then(|t| int(t.get("total_tokens").unwrap_or(&missing)));
        let calls = totals.and_then(|t| int(t.get("calls").unwrap_or(&missing)));
        let cache_rate = totals.and_then(|t| {
            cache_rate(
                t.get("cache_hit_tokens").and_then(num),
                t.get("cache_miss_tokens").and_then(num),
                t.get("prompt_tokens").and_then(num),
            )
        });
        let total_seconds = stats.timing.get("total_seconds").and_then(num);
        // True output rate over timed calls only: historical tokens lack
        // wall-clock time, so dividing them by fresh milliseconds would
        // inflate the rate by orders of magnitude.
        let timed_ms = totals.and_then(|t| int(t.get("completion_ms").unwrap_or(&missing)));
        let timed_tokens = totals.and_then(|t| int(t.get("timed_completion_tokens").unwrap_or(&missing)));
        let tokens_per_second = match (timed_tokens, timed_ms) {
            (Some(t), Some(ms)) if ms > 0 => Some(t as f64 / (ms as f64 / 1000.0)),
            _ => None,
        };
        Self { total_tokens, calls, cache_rate, total_seconds, tokens_per_second }
    }
}

/// Sort key for usage rows: total tokens desc, then id asc (mirrors `usageRows`).
pub fn usage_rows_for_group(usage: &serde_json::Value, group: UsageGroup) -> Vec<(String, UsageRow)> {
    let slots = usage
        .get(group.key())
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let labels = usage.get("labels").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let mut rows: Vec<(String, UsageRow)> = Vec::new();
    for (id, slot) in slots.iter() {
        let label = labels
            .get(id)
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                if group == UsageGroup::ByStage {
                    stage_label(id)
                } else {
                    id.clone()
                }
            });
        let prompt = slot.get("prompt_tokens").and_then(num).unwrap_or(0.0);
        let completion = slot.get("completion_tokens").and_then(num).unwrap_or(0.0);
        let total = slot
            .get("total_tokens")
            .and_then(num)
            .unwrap_or(prompt + completion);
        // Merge same-label rows exactly like `usageRows` (model ledgers split
        // by inference options share a display name).
        if let Some(existing) = rows.iter_mut().find(|(_, r)| r.label == label) {
            existing.1.prompt += prompt;
            existing.1.completion += completion;
            existing.1.total += total;
            existing.1.cache_hit += slot.get("cache_hit_tokens").and_then(num).unwrap_or(0.0);
            existing.1.cache_miss += slot.get("cache_miss_tokens").and_then(num).unwrap_or(0.0);
            existing.1.calls += slot.get("calls").and_then(int).unwrap_or(0);
            continue;
        }
        rows.push((
            id.clone(),
            UsageRow {
                label,
                total,
                prompt,
                completion,
                cache_hit: slot.get("cache_hit_tokens").and_then(num).unwrap_or(0.0),
                cache_miss: slot.get("cache_miss_tokens").and_then(num).unwrap_or(0.0),
                calls: slot.get("calls").and_then(int).unwrap_or(0),
            },
        ));
    }
    rows.sort_by(|a, b| {
        b.1.total
            .partial_cmp(&a.1.total)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    rows
}

/// `timingRuns` from accountingData.ts: runs with string ids, newest first
/// by `started_at`.
pub fn timing_runs(timing: &serde_json::Value) -> Vec<RunRow> {
    let mut runs: Vec<RunRow> = timing
        .get("runs")
        .and_then(|r| r.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|run| {
                    let id = run.get("id")?.as_str()?;
                    Some(RunRow {
                        id: id.to_string(),
                        operation: str_field(run, "operation"),
                        status: str_field(run, "status"),
                        started_at: str_field(run, "started_at"),
                        seconds: run.get("elapsed_seconds").and_then(num).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| a.id.cmp(&b.id)));
    runs
}

/// Stage id → readable label. The web app resolves `operationLabel(id, t)`;
/// the native view keeps the same English source strings.
fn stage_label(id: &str) -> String {
    match id {
        "parse" => "Source parsing".into(),
        "prepare" => "Preparation".into(),
        "translation" => "Book translation".into(),
        "chapter_translation" => "Chapter translation".into(),
        "review" => "Whole-book review".into(),
        "srt" => "Subtitle translation".into(),
        other => other.to_string(),
    }
}

/// Operation id → readable label for the run chart (same source).
pub fn operation_label(op: &str) -> String {
    if op.is_empty() {
        return "Run".into();
    }
    stage_label(op)
}

/// `started_at` ISO → short "Sep 17, 21:32" label. Falls back to the raw
/// string prefix (date part) when parsing fails.
pub fn short_time(started_at: &str) -> String {
    if started_at.is_empty() {
        return "—".into();
    }
    // Parse "2026-09-17T21:32:00Z" manually (no extra dep).
    let date_part = started_at.split('T').next().unwrap_or("");
    let time_part = started_at.split('T').nth(1).unwrap_or("");
    let dp: Vec<&str> = date_part.split('-').collect();
    let (month, day) = match dp.as_slice() {
        [_, m, d, ..] | [m, d] => (*m, *d),
        _ => return started_at.chars().take(16).collect(),
    };
    let month_name = match month {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => month,
    };
    let hm: Vec<&str> = time_part.split(':').collect();
    let hm = match hm.as_slice() {
        [h, m, ..] => format!("{h}:{m}"),
        _ => String::new(),
    };
    let day = day.trim_start_matches('0');
    if hm.is_empty() {
        format!("{month_name} {day}")
    } else {
        format!("{month_name} {day}, {hm}")
    }
}

// ---------------------------------------------------------------------------
// Elements
// ---------------------------------------------------------------------------

/// One headline metric card (color dot + label + value).
fn metric_card(label: &str, value: &str, dot: Hsla, palette: &theme::Palette) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(div().w(px(8.0)).h(px(8.0)).rounded_lg().bg(dot))
                .child(label.to_string()),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value.to_string()),
        )
}

/// Headline metric row: cumulative tokens / requests / cache hit rate /
/// recorded time / avg tokens per second.
pub fn metric_cards(metrics: &UsageMetrics, palette: &theme::Palette) -> impl IntoElement {
    let rate = metrics.cache_rate.map(|r| format!("{:.1}%", r * 100.0)).unwrap_or_else(|| "—".into());
    let tps = metrics.tokens_per_second.map(|t| format!("{:.0} t/s", t)).unwrap_or_else(|| "—".into());
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap_4()
        .p_4()
        .rounded_md()
        .bg(palette.muted)
        .child(metric_card("Cumulative tokens", &format_count(metrics.total_tokens), palette.primary, palette))
        .child(metric_card("Model requests", &format_count(metrics.calls), palette.accent_foreground, palette))
        .child(metric_card("Cache hit rate", &rate, palette.secondary_foreground, palette))
        .child(metric_card("Recorded time", &format_duration(metrics.total_seconds), palette.card_foreground, palette))
        .child(metric_card("Avg tokens/s", &tps, palette.success, palette))
}

/// Segmented legend entry under a usage bar.
fn legend_dot(color: Hsla, text: String, palette: &theme::Palette) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(palette.muted_foreground)
        .child(div().w(px(8.0)).h(px(8.0)).rounded_lg().bg(color))
        .child(text)
}

/// Stacked token bar for one usage row (div-based; proportional segments for
/// cached / uncached / unknown / output / other tokens on a muted track,
/// scaled to the group maximum like `TokenBar` in UsageChart.tsx).
fn usage_bar(row: &UsageRow, maximum: f64, palette: &theme::Palette) -> impl IntoElement {
    let scale = maximum.max(1.0);
    let input = row.prompt;
    let output = row.completion;
    let reported = row.cache_hit + row.cache_miss;
    let valid_cache = reported <= input;
    let cached = if valid_cache { row.cache_hit } else { 0.0 };
    let uncached = if valid_cache { row.cache_miss } else { 0.0 };
    let unknown = if valid_cache {
        (input - reported).max(0.0)
    } else {
        input
    };
    let other = (row.total - input - output).max(0.0);
    let segs: [(f64, Hsla); 5] = [
        (cached, palette.success),
        (uncached, palette.info),
        (unknown, palette.muted_foreground),
        (output, palette.warning),
        (other, palette.secondary_foreground),
    ];
    let mut bar = div()
        .flex()
        .flex_row()
        .h(px(10.0))
        .flex_1()
        .rounded_lg()
        .bg(palette.muted)
        .overflow_hidden();
    for (value, color) in segs {
        let frac = ((value / scale).clamp(0.0, 1.0)) as f32;
        if frac > 0.005 {
            bar = bar.child(div().h_full().bg(color).w(relative(frac)));
        }
    }
    bar
}

/// Usage breakdown rows: label + stacked token bar + totals (tokens, calls,
/// cache hit-rate), with a segment legend on top. Shows the top 8 rows by
/// tokens (mirrors the source cap).
/// The group tab strip lives in `progress.rs` (needs `cx.listener`).
pub fn usage_rows(usage: &serde_json::Value, group: UsageGroup, palette: &theme::Palette) -> impl IntoElement {
    let rows = usage_rows_for_group(usage, group);
    let maximum = rows.iter().map(|(_, r)| r.total).fold(1.0_f64, f64::max);
    let mut list = div().flex().flex_col().gap_3();
    if rows.is_empty() {
        list = list.child(
            div()
                .py_4()
                .text_center()
                .text_sm()
                .text_color(palette.muted_foreground)
                .child("No usage yet"),
        );
    }
    for (_, row) in rows.iter().take(8) {
        let rate = cache_rate(Some(row.cache_hit), Some(row.cache_miss), Some(row.prompt))
            .map(|r| format!("{:.0}% cached", r * 100.0))
            .unwrap_or_default();
        list = list.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .justify_between()
                        .gap_2()
                        .text_sm()
                        .child(div().font_weight(FontWeight::MEDIUM).child(row.label.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(palette.muted_foreground)
                                .child(format!(
                                    "{} tokens · {} calls{}",
                                    format_count(Some(row.total as u64)),
                                    format_count(Some(row.calls)),
                                    if rate.is_empty() { String::new() } else { format!(" · {rate}") },
                                )),
                        ),
                )
                .child(usage_bar(row, maximum, palette)),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Usage breakdown"))
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap_3()
                .child(legend_dot(palette.success, "Cached input".into(), palette))
                .child(legend_dot(palette.info, "Uncached input".into(), palette))
                .child(legend_dot(palette.muted_foreground, "Other input".into(), palette))
                .child(legend_dot(palette.warning, "Output".into(), palette))
                .child(legend_dot(palette.secondary_foreground, "Other".into(), palette)),
        )
        .child(list)
}

#[derive(Clone)]
struct ShareDatum {
    label: SharedString,
    total: f64,
    color: Hsla,
}

/// Distinct categorical palette for share charts (stable order, Light-safe).
fn share_colors(palette: &theme::Palette) -> [Hsla; 8] {
    [
        palette.primary,
        palette.success,
        palette.warning,
        palette.info,
        palette.destructive,
        palette.accent_foreground,
        palette.muted_foreground,
        palette.secondary_foreground,
    ]
}

/// Model share donut: each model slice sized by total tokens, with a legend
/// carrying exact values. Answers "which model did the work" at a glance.
/// Empty → "No usage yet".
pub fn usage_donut(usage: &serde_json::Value, palette: &theme::Palette) -> impl IntoElement {
    let rows = usage_rows_for_group(usage, UsageGroup::ByModel);
    let colors = share_colors(palette);
    let mut legend = div().flex().flex_col().gap_1();
    if rows.is_empty() {
        legend = legend.child(
            div().text_sm().text_color(palette.muted_foreground).child("No usage yet"),
        );
    }
    let data: Vec<ShareDatum> = rows
        .iter()
        .take(8)
        .enumerate()
        .map(|(i, (_, r))| ShareDatum {
            label: r.label.clone().into(),
            total: r.total,
            color: colors[i % colors.len()],
        })
        .collect();
    for d in &data {
        legend = legend.child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(div().w(px(10.0)).h(px(10.0)).rounded_sm().bg(d.color).flex_none())
                        .child(div().truncate().child(d.label.clone())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child(format_count(Some(d.total as u64))),
                ),
        );
    }
    let total: f64 = rows.iter().map(|(_, r)| r.total).sum();
    let chart = PieChart::new(data)
        .id("usage-model-donut")
        .value(|d| d.total.max(0.0) as f32)
        .color(|d| d.color)
        // Radii are absolute pixels (outer resolves to height * 0.4 = 104).
        .inner_radius(62.0)
        .pad_angle(0.03)
        .appear(false);
    // Vertical stack: donut on top (centered), legend below at full width
    // so long model names stay readable instead of truncating.
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Usage by model"))
        .child(
            div()
                .flex()
                .justify_center()
                .child(div().w(px(260.0)).h(px(260.0)).flex_none().child(chart)),
        )
        .child(legend)
        .child(
            div().text_xs().text_color(palette.muted_foreground).child(format!(
                "{} tokens across {} models",
                format_count(Some(total as u64)),
                rows.len()
            )),
        )
}

/// Provider share donut: same polished ring as [`usage_donut`] but over the
/// provider ledger. With 2–3 providers a donut fills the panel and reads
/// better than near-empty ranking bars. Empty → "No usage yet".
pub fn usage_provider_donut(usage: &serde_json::Value, palette: &theme::Palette) -> impl IntoElement {
    let rows = usage_rows_for_group(usage, UsageGroup::ByProvider);
    let colors = share_colors(palette);
    let mut legend = div().flex().flex_col().gap_2();
    if rows.is_empty() {
        legend = legend.child(
            div().text_sm().text_color(palette.muted_foreground).child("No usage yet"),
        );
    }
    let data: Vec<ShareDatum> = rows
        .iter()
        .take(8)
        .enumerate()
        .map(|(i, (_, r))| ShareDatum {
            label: r.label.clone().into(),
            total: r.total,
            color: colors[i % colors.len()],
        })
        .collect();
    let total: f64 = rows.iter().map(|(_, r)| r.total).sum();
    for d in &data {
        let pct = if total > 0.0 { d.total / total * 100.0 } else { 0.0 };
        legend = legend.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .child(div().w(px(10.0)).h(px(10.0)).rounded_sm().bg(d.color).flex_none())
                                .child(div().truncate().font_weight(FontWeight::MEDIUM).child(d.label.clone())),
                        )
                        .child(
                            div().text_sm().font_weight(FontWeight::SEMIBOLD).child(format!("{pct:.1}%")),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child(format!("{} tokens", format_count(Some(d.total as u64)))),
                ),
        );
    }
    let chart = PieChart::new(data)
        .id("usage-provider-donut")
        .value(|d| d.total.max(0.0) as f32)
        .color(|d| d.color)
        // Radii are absolute pixels (outer resolves to height * 0.4 = 104).
        .inner_radius(62.0)
        .pad_angle(0.03)
        .appear(false);
    div()
        // Vertical stack like `usage_donut`: ring on top, full-width legend below.
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Usage by provider"))
        .child(
            div()
                .flex()
                .justify_center()
                .child(div().w(px(260.0)).h(px(260.0)).flex_none().child(chart)),
        )
        .child(legend)
        .child(
            div().text_xs().text_color(palette.muted_foreground).child(format!(
                "{} tokens across {} providers",
                format_count(Some(total as u64)),
                rows.len()
            )),
        )
}

/// Run-history list in a fixed-height box with internal scroll (same pattern
/// as the chapter panel): all runs render, the box never grows, the page
/// layout never shifts. Empty → "No runs yet".
pub fn run_chart(runs: &[RunRow], palette: &theme::Palette) -> impl IntoElement {
    let section = div().flex().flex_col().gap_2().child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Run history"))
            .child(
                div()
                    .text_xs()
                    .text_color(palette.muted_foreground)
                    .child(format!("{} runs", runs.len())),
            ),
    );
    if runs.is_empty() {
        return section.child(
            div()
                .py_4()
                .text_center()
                .text_sm()
                .text_color(palette.muted_foreground)
                .child("No runs yet"),
        );
    }
    let maximum = runs.iter().map(|r| r.seconds).fold(1.0_f64, f64::max);
    let mut list = div().flex().flex_col().gap_2();
    for run in runs.iter() {
        let frac = ((run.seconds / maximum).clamp(0.0, 1.0)) as f32;
        let op = operation_label(&run.operation);
        let tone = match run.status.as_str() {
            "completed" | "done" | "ok" => palette.success,
            "failed" | "error" => palette.destructive,
            "running" => palette.info,
            _ => palette.muted_foreground,
        };
        list = list.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_sm()
                        .child(div().font_weight(FontWeight::MEDIUM).child(op))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(format_duration(Some(run.seconds)))),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .h(px(6.0))
                        .rounded_lg()
                        .bg(palette.muted)
                        .overflow_hidden()
                        .child(div().h_full().bg(tone).w(relative(frac))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child(short_time(&run.started_at))
                        .child(div().text_color(tone).child(run.status.clone())),
                ),
        );
    }
    // Fixed height like the chapter panel: the box never grows, extra rows
    // scroll inside (max_h would still grow the page with many rows).
    section.child(div().h(px(320.0)).overflow_y_scrollbar().child(list))
}

#[derive(Clone)]
struct TimeDatum {
    label: SharedString,
    seconds: f64,
}

/// Compact over-time strip: per-run elapsed-seconds line in a short band
/// with thinned ticks — trend context without page dominance. The usage
/// ledger has no per-timestamp token series, so each run is one point.
pub fn usage_over_time_compact(runs: &[RunRow], palette: &theme::Palette) -> impl IntoElement {
    let section = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Run durations"))
                .child(
                    div().text_xs().text_color(palette.muted_foreground).child(format!(
                        "{} runs",
                        runs.len()
                    )),
                ),
        );
    if runs.is_empty() {
        return section.child(
            div().text_sm().text_color(palette.muted_foreground).child("No data yet"),
        );
    }
    let data: Vec<TimeDatum> = runs
        .iter()
        .rev()
        .take(12)
        .rev()
        .map(|r| TimeDatum { label: short_time(&r.started_at).into(), seconds: r.seconds })
        .collect();
    let chart = LineChart::new(data)
        .id("progress-usage-time-compact")
        .x(|d| d.label.clone())
        .y(|d| d.seconds as f32)
        .stroke(palette.primary)
        .grid(false)
        .x_tick_count(4)
        .appear(false);
    section.child(div().h(px(80.0)).child(chart))
}

#[cfg(test)]
mod tests {
    use super::{cache_rate, format_count, format_duration, timing_runs, usage_rows_for_group, UsageGroup, UsageMetrics, UsageRow};

    fn stats(usage: serde_json::Value, timing: serde_json::Value) -> crate::client::ProjectStats {
        crate::client::ProjectStats { usage, timing, live: None }
    }

    #[test]
    fn metrics_map_to_documented_paths() {
        let s = stats(
            serde_json::json!({
                "totals": {
                    "calls": 37,
                    "total_tokens": 496861,
                    "prompt_tokens": 1000,
                    "cache_hit_tokens": 356,
                    "cache_miss_tokens": 644,
                }
            }),
            serde_json::json!({ "total_seconds": 133.0 }),
        );
        let m = UsageMetrics::from_stats(&s, false);
        assert_eq!(m.total_tokens, Some(496861));
        assert_eq!(m.calls, Some(37));
        assert!((m.cache_rate.unwrap() - 0.356).abs() < 1e-9);
        assert_eq!(m.total_seconds, Some(133.0));
    }

    #[test]
    fn cache_rate_guards_match_source() {
        // No cache traffic → undefined.
        assert_eq!(cache_rate(Some(0.0), Some(0.0), Some(10.0)), None);
        // Cache traffic exceeding prompt → undefined (cannot infer miss).
        assert_eq!(cache_rate(Some(6.0), Some(6.0), Some(10.0)), None);
        // Missing values → undefined.
        assert_eq!(cache_rate(None, Some(1.0), Some(10.0)), None);
    }

    #[test]
    fn format_duration_matches_source() {
        assert_eq!(format_duration(None), "—");
        assert_eq!(format_duration(Some(4.1)), "4.1 s");
        assert_eq!(format_duration(Some(133.0)), "2m 13s");
        assert_eq!(format_duration(Some(3723.0)), "1h 2m 3s");
    }

    #[test]
    fn format_count_groups_thousands() {
        assert_eq!(format_count(Some(496861)), "496,861");
        assert_eq!(format_count(None), "—");
        assert_eq!(format_count(Some(0)), "0");
    }

    #[test]
    fn usage_rows_sort_and_merge_like_source() {
        let usage = serde_json::json!({
            "by_stage": {
                "a": {"total_tokens": 10, "prompt_tokens": 6, "completion_tokens": 4, "calls": 1},
                "b": {"total_tokens": 50, "prompt_tokens": 30, "completion_tokens": 20, "calls": 2},
            },
            "labels": {},
        });
        let rows = usage_rows_for_group(&usage, UsageGroup::ByStage);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "b");
        assert_eq!(rows[0].1.total, 50.0);
    }

    #[test]
    fn timing_runs_newest_first() {
        let timing = serde_json::json!({
            "runs": [
                {"id": "old", "started_at": "2026-09-17T21:03:00Z", "elapsed_seconds": 962.0},
                {"id": "new", "started_at": "2026-09-17T21:32:00Z", "elapsed_seconds": 366.0},
                {"id": 7},
            ],
        });
        let runs = timing_runs(&timing);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "new");
    }
}
