#!/usr/bin/env python3
"""Differential check: emco_rules.py and the Rust port answer the same.

Generates random programs and spindle queries, asks both, and compares the
answers exactly (line numbers, severities, message text). The fixed unit
tests cover the cases someone thought of; this covers the scanner's edges.

    python3 kd240/web/rules/diff_against_python.py [count] [seed]
"""
import json
import os
import random
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
sys.path.insert(0, os.path.join(REPO, "linuxcnc", "sim", "emco-pcturn50", "qtvcp", "screens", "emco_lathe"))
import emco_rules as R  # noqa: E402

WORDS = ["G0", "G1", "G7", "G8", "G20", "G21", "G53", "G96", "G97", "G33", "G33.1", "G76", "G10", "G28",
         "G30", "G43", "G43.1", "G92", "M3", "M4", "M5", "M6", "M2", "M30", "T1", "T3",
         "X48", "X96", "X47.9995", "X14", "X[#1]", "X#2", "X.5", "X-3.", "Z0", "Z-10", "Z0.0004", "Z 1",
         "S800", "S1400", "S500", "S350", "S5000", "S0", "S-5", "D2000", "P1", "L20", "F100", "I-1", "K0.6"]
ODD = ["(", ")", ";", "( Stock 12 )", "(tool t1)", "o<r> sub", "o<r> endsub", "O100", "o<r> call [1]",
       "\r", "\t", "\x1c", " ", "\x0b", "  ", "1..2", "X1#", "g0 x96", "(a;b)", "\x85", "é"]


def program(rng):
    lines = []
    for _ in range(rng.randint(0, 14)):
        parts = [rng.choice(WORDS) for _ in range(rng.randint(0, 5))]
        if rng.random() < 0.3:
            parts.insert(rng.randrange(len(parts) + 1), rng.choice(ODD))
        lines.append((" " if rng.random() < 0.7 else "").join(parts))
    return rng.choice(["\n", "\r\n", "\r"]).join(lines) + rng.choice(["", "\n"])


def python_answer(q):
    if "preflight" in q:
        x, z, t = q["machine"]
        return [[f.line, f.severity, f.message]
                for f in R.preflight(q["preflight"], q["belt"], R.Machine(x, z, t))]
    if "spindle" in q:
        return R.spindle_violation(q["belt"], q["spindle"], q["threading"])
    if "gate" in q:
        g = R.ReferenceGate()
        return [[g.update(e, on, h), g.position_lost] for e, on, h in q["gate"]]
    return R.clamp_rpm(q["belt"], q["clamp"])


def main():
    count = int(sys.argv[1]) if len(sys.argv) > 1 else 3000
    rng = random.Random(int(sys.argv[2]) if len(sys.argv) > 2 else 1)
    queries = []
    for name in ("tutorial-part.ngc", "stepped-shaft.ngc"):
        with open(os.path.join(REPO, "gcode", name)) as f:
            text = f.read()
        for belt in (1, 2, 3):
            for m in ([48.0, 0.0, True], [48.0, 0.0, False], [24.0, -1.5, True]):
                queries.append({"preflight": text, "belt": belt, "machine": m})
    for _ in range(count):
        belt = rng.randint(1, 3)
        r = rng.random()
        if r < 0.8:
            m = rng.choice([[48.0, 0.0, True], [48.0, 0.0, False], [24.0, -1.5, True]])
            queries.append({"preflight": program(rng), "belt": belt, "machine": m})
        elif r < 0.95:
            rpm = rng.choice([0, 1, 129.5, 130, 1300, 1300.4, 1300.5, 2000, 400, 400.4, 400.5, 401, -800, 3001,
                              rng.uniform(-4000, 4000)])
            queries.append({"spindle": rpm, "belt": belt, "threading": rng.random() < 0.5})
        elif r < 0.97:
            queries.append({"gate": [[rng.random() < 0.2, rng.random() < 0.8,
                                      [rng.random() < 0.7 for _ in range(rng.randint(0, 3))]]
                                     for _ in range(rng.randint(1, 12))], "belt": belt})
        else:
            queries.append({"clamp": rng.uniform(-100, 4000), "belt": belt})
    rust = subprocess.run(
        ["cargo", "run", "-q", "--example", "rules_json"], cwd=HERE, check=True,
        input="".join(json.dumps(q) + "\n" for q in queries), capture_output=True, text=True).stdout.splitlines()
    assert len(rust) == len(queries), (len(rust), len(queries))
    bad = 0
    for q, r in zip(queries, rust):
        want = python_answer(q)
        got = json.loads(r)
        if isinstance(want, float) or isinstance(want, int) and not isinstance(want, bool):
            same = float(want) == float(got)
        else:
            same = want == got
        if not same:
            bad += 1
            if bad <= 5:
                print("MISMATCH", json.dumps(q), "\n  python:", want, "\n  rust:  ", got)
    findings = sum(len(python_answer(q)) for q in queries if "preflight" in q)
    print("{} queries ({} preflight findings), {} mismatches".format(len(queries), findings, bad))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
