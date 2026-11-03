"""Summarize hyperfine JSON files in a results dir into SUMMARY.md (stdlib only)."""

import json
import sys
from pathlib import Path


def _ms(seconds: float) -> str:
    return f"{seconds * 1000:.2f}"


def _flat(r: dict) -> dict:
    """hyperfine 2.x nests stats under summary.time_wall_clock; 1.x has them at top level."""
    return r.get('summary', {}).get('time_wall_clock', r)


def _rss(path: Path) -> dict[str, int]:
    out: dict[str, int] = {}
    if path.is_file():
        for line in path.read_text().splitlines():
            side, _, kb = line.partition(" ")
            if kb.strip().isdigit():
                out[side] = int(kb)
    return out


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: summarize.py RESULTS_DIR", file=sys.stderr)
        return 2
    d = Path(sys.argv[1])
    rows = [
        "| Pair | Rust mean ± σ (ms) | Python mean ± σ (ms) | Speedup (Py/Rs) | RSS Rust / Python (MiB) |",
        "|---|---|---|---|---|",
    ]
    for f in sorted(d.glob("*.json")):
        results = json.loads(f.read_text())["results"]
        by_name = {r.get("name", r["command"]): _flat(r) for r in results}
        rs, py = by_name.get("rust"), by_name.get("python")
        if rs is None or py is None:
            continue
        rss = _rss(f.with_suffix(".rss"))
        rss_cell = (
            f"{rss['rust'] / 1024:.1f} / {rss['python'] / 1024:.1f}"
            if len(rss) == 2
            else "n/a"
        )
        rows.append(
            f"| {f.stem} | {_ms(rs['mean'])} ± {_ms(rs['stddev'])} "
            f"| {_ms(py['mean'])} ± {_ms(py['stddev'])} "
            f"| {py['mean'] / rs['mean']:.1f}x | {rss_cell} |"
        )
    text = "\n".join(rows) + "\n"
    skipped = d / "skipped.txt"
    if skipped.is_file():
        text += "\nSkipped:\n\n" + "".join(
            f"- {s}\n" for s in skipped.read_text().splitlines()
        )
    (d / "SUMMARY.md").write_text(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
