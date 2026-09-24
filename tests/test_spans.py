"""Span-model contracts: union math and the summary line.

Synthetic data only (repo HARD RULE #1: no personal/local context in fixtures).
"""

from session_timeline.spans import SessionTimeline, Span, SpanKind, union_duration


def test_union_does_not_double_count_overlaps():
    # Two parallel tool spans overlapping 10s must union to 20s, not 30s.
    spans = [
        Span(SpanKind.TOOL, 0.0, 20.0, "terminal"),
        Span(SpanKind.TOOL, 10.0, 30.0, "read_file"),
    ]
    assert union_duration(spans) == 30.0


def test_union_sums_disjoint_spans():
    spans = [
        Span(SpanKind.INFERENCE, 0.0, 5.0),
        Span(SpanKind.INFERENCE, 7.0, 9.0),
    ]
    assert union_duration(spans) == 7.0


def test_union_empty():
    assert union_duration([]) == 0.0


def test_summary_line_mentions_every_phase_and_top_tool():
    tl = SessionTimeline(
        session_id="synthetic-1",
        spans=[
            Span(SpanKind.INFERENCE, 0.0, 660.0),
            Span(SpanKind.TOOL, 660.0, 1200.0, "terminal"),
            Span(SpanKind.TOOL, 900.0, 1000.0, "read_file"),
            Span(SpanKind.IDLE, 1200.0, 1620.0),
        ],
    )
    line = tl.summary_line()
    assert "inference 11min" in line
    assert "tools 9min" in line
    assert "waiting-for-you 7min" in line
    assert "terminal" in line


def test_spans_are_non_negative_by_contract():
    tl = SessionTimeline(
        session_id="synthetic-2",
        spans=[Span(SpanKind.TOOL, 5.0, 5.0)],
    )
    assert tl.spans[0].duration == 0.0
