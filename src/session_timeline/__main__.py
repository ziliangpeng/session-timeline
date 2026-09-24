"""CLI entry: python -m session_timeline <converter> --session <id>."""

from __future__ import annotations

import argparse
import sys


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="session-timeline",
        description="Reconstruct an agent-session timeline (inference / tools / waiting-for-user) from existing artifacts.",
    )
    sub = parser.add_subparsers(dest="converter", required=True)

    p_prime = sub.add_parser("prime", help="Prime Agent per-session JSONL")
    p_prime.add_argument("--session", required=True, help="session file path or id")
    p_prime.add_argument("--out", default="trace.json", help="output Perfetto trace-event JSON path")

    p_hermes = sub.add_parser("hermes", help="Hermes Agent profile state.db + logs")
    p_hermes.add_argument("--session", required=True, help="session id")
    p_hermes.add_argument("--home", default=None, help="profile home (default: ~/.hermes)")
    p_hermes.add_argument("--out", default="trace.json", help="output Perfetto trace-event JSON path")

    args = parser.parse_args(argv)

    if args.converter == "prime":
        print("prime converter: not yet implemented (v0 skeleton)", file=sys.stderr)
        return 2
    if args.converter == "hermes":
        print("hermes converter: not yet implemented (v0 skeleton)", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
