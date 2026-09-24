"""Unified span model shared by every converter and renderer."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum


class SpanKind(str, Enum):
    INFERENCE = "inference"
    TOOL = "tool"
    IDLE = "idle"  # waiting for the user — first-class, never folded into other kinds
    SUBAGENT = "subagent"


@dataclass
class Span:
    """One half-open interval [t_start, t_end) on a session's timeline."""

    kind: SpanKind
    t_start: float
    t_end: float
    label: str = ""
    meta: dict = field(default_factory=dict)

    @property
    def duration(self) -> float:
        return self.t_end - self.t_start


def union_duration(spans: list[Span]) -> float:
    """Total wall time covered by possibly-overlapping spans.

    Parallel tool calls overlap; attribution must union, never sum — naive
    subtraction double-counts. Merging by endpoint sort is O(n log n).
    """
    if not spans:
        return 0.0
    total = 0.0
    cur_start: float | None = None
    cur_end: float | None = None
    for s in sorted(spans, key=lambda x: (x.t_start, x.t_end)):
        if cur_start is None or cur_end is None or s.t_start > cur_end:
            if cur_start is not None and cur_end is not None:
                total += cur_end - cur_start
            cur_start, cur_end = s.t_start, s.t_end
        else:
            cur_end = max(cur_end, s.t_end)
    assert cur_start is not None and cur_end is not None
    total += cur_end - cur_start
    return total


@dataclass
class SessionTimeline:
    """A session reduced to spans; renderers consume only this."""

    session_id: str
    spans: list[Span] = field(default_factory=list)
    started_at: float | None = None
    ended_at: float | None = None

    def summary_line(self) -> str:
        """The one-line time attribution — a first-class deliverable.

        `session 32min = inference 11min / tools 14min (terminal 9min) / waiting-for-you 7min`
        """
        def fmt(sec: float) -> str:
            if sec >= 3600:
                return f"{sec / 3600:.1f}h"
            if sec >= 60:
                return f"{sec / 60:.0f}min"
            return f"{sec:.0f}s"

        tools = [s for s in self.spans if s.kind is SpanKind.TOOL]
        by_kind = {
            "inference": union_duration([s for s in self.spans if s.kind is SpanKind.INFERENCE]),
            "tools": union_duration(tools),
            "waiting-for-you": union_duration([s for s in self.spans if s.kind is SpanKind.IDLE]),
        }
        total = sum(by_kind.values())
        top_tools: dict[str, float] = {}
        for s in tools:
            top_tools[s.label or s.meta.get("tool", "?")] = (
                top_tools.get(s.label or s.meta.get("tool", "?"), 0.0) + s.duration
            )
        top = max(top_tools, key=top_tools.get) if top_tools else None  # type: ignore[arg-type]
        detail = f" ({top} {fmt(top_tools[top])})" if top else ""
        return (
            f"session {fmt(total)} = inference {fmt(by_kind['inference'])} / "
            f"tools {fmt(by_kind['tools'])}{detail} / waiting-for-you {fmt(by_kind['waiting-for-you'])}"
        )
