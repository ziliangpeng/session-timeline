//! Unified session model — the contract every loader emits (spec/overview.md).
//!
//! Schema starts loose (spec Q3 ruling): most fields optional; consumers must
//! degrade gracefully. Loaders are the only harness-specific code; everything
//! above this model is harness-agnostic.

use serde::{Deserialize, Serialize};

/// Span kinds. `Idle` is first-class: waiting-for-user is a deliverable, never
/// folded into other kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpanKind {
    Inference,
    Tool,
    Idle,
}

/// Silence longer than this between attributed events is idle ("session left
/// open / dormant"), never inference. Spec: spans.py GAP_CAP_S.
pub const GAP_CAP_S: f64 = 300.0;

/// Preview cap for meta strings (args/result/output/next_user).
pub const PREVIEW_N: usize = 220;

/// One half-open interval `[t_start, t_end)` in Unix epoch seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub kind: SpanKind,
    pub t_start: f64,
    pub t_end: f64,
    /// Short label: tool name for TOOL, "model" for INFERENCE, "idle" for IDLE.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Open-ended payload (`args`, `result`, `output`, `next_user`, `note`).
    /// Consumers treat unknown and missing keys the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

/// Previewed strings for hover/inspect UI. All fields optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Human-pace classification, derived by the loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// A human conversed in this session.
    Human,
    /// No human input: subagent children, autonomous runs.
    Sub,
}

/// The unified session object. Required: `id`, `kind`, time extent, spans.
/// Everything else is optional — loaders fill what the source has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Stable unique id, namespaced by loader (e.g. "hermes:20260924_...").
    pub id: String,
    pub kind: SessionKind,
    pub t_start: f64,
    pub t_end: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Harness-specific origin label (Hermes profile name), display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Entry-point label (tui / cli / cron / …), display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub spans: Vec<Span>,
}

impl Session {
    /// Wall time covered by possibly-overlapping spans of one kind.
    /// Parallel tool calls overlap; attribution must union, never sum.
    pub fn union_duration(&self, kind: SpanKind) -> f64 {
        let mut iv: Vec<(f64, f64)> = self
            .spans
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| (s.t_start, s.t_end))
            .collect();
        iv.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let mut total = 0.0f64;
        let mut cur: Option<(f64, f64)> = None;
        for (a, b) in iv {
            match cur {
                Some((cs, ce)) if a <= ce => cur = Some((cs, ce.max(b))),
                Some((cs, ce)) => {
                    total += ce - cs;
                    cur = Some((a, b));
                }
                None => cur = Some((a, b)),
            }
        }
        if let Some((cs, ce)) = cur {
            total += ce - cs;
        }
        total
    }
}
