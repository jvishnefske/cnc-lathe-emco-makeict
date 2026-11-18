#!/usr/bin/env python3
"""Read WinNC's own RS-485 message log ("Aufzeichnung der RS485 Schnittstelle").

EMCO's WinNC can record every message it exchanges with the control over its
RS-485 link. One such recording is public: `485tr-1.txt`, posted to the
LinuxCNC forum from a **PC Mill 50** (a sibling of the PC Turn 50 with the same
generation of control; see docs/31-rs485-message-layer.md for what carries
over and what doesn't). It's already decoded at the message level by WinNC
itself, so it shows the link's message layer without a capture of the wire:

    O ------ 485 Out       Nr.:     2   Zeit:    0.000 ------
       L=  6   Z=  0   Q=250   Code= 50=2
        0D 61                                                   .a
    I ------ 485 In        Nr.:     1   Zeit:    0.055 ------
       L=  7   Z=250   Q=  0   FCode= 527 .. AC-Eingaenge
        C8 00                                                   ..

`O`/`I` is the PC's direction, `Nr.` a per-direction sequence number, `Zeit`
seconds since the recording started, `L` the message length, `Z` the
destination (Ziel) and `Q` the source (Quelle) address. Outbound messages carry
a one-character code (shown as its decimal value and the character); inbound
ones a function code (FCode) in the 500s with WinNC's own name for it.

    python3 tools/winnc-485-log.py 485tr-1.txt            # the code table (Markdown)
    python3 tools/winnc-485-log.py 485tr-1.txt --dump     # one line per message
    python3 tools/winnc-485-log.py 485tr-1.txt --json     # every message, for other tools

This is a reader for a text file. It opens no serial port and sends nothing.
"""

import argparse
import json
import re
import sys
from collections import OrderedDict
from dataclasses import asdict, dataclass, field

# Addresses seen in the recording. The names are WinNC's (its FCode labels say
# "AC-…" for the axis controller and "SPS-…" for the PLC); the PC is the
# source of every outbound message.
ADDRESSES = {250: "PC", 0: "AC", 16: "SPS"}

_HEADER = re.compile(
    r"^(?P<dir>[OI]) -+ 485 (?:Out|In)\s+Nr\.:\s*(?P<nr>\d+)\s+Zeit:\s*(?P<t>[\d.]+) -+\s*$")
_FIELDS = re.compile(
    r"^\s+L=\s*(?P<l>\d+)\s+Z=\s*(?P<z>\d+)\s+Q=\s*(?P<q>\d+)\s+"
    r"(?:Code=\s*(?P<code>\d+)=(?P<ch>.?)|FCode=\s*(?P<fcode>\d+) \.\.\s?(?P<name>.*?))\s*$")
# Up to sixteen hex bytes in groups of four, one or two spaces apart; the
# ASCII column after them is six or more spaces away, so it never matches.
_HEX = re.compile(r"^ {4}((?:[0-9A-F]{2} {1,2})*[0-9A-F]{2})(?: {3,}.*)?$")


@dataclass
class Message:
    direction: str          # "out" (PC -> control) or "in" (control -> PC)
    nr: int                 # WinNC's per-direction sequence number
    time: float             # seconds since the recording started
    length: int             # L, as WinNC prints it
    dest: int               # Z
    src: int                # Q
    code: int               # the outbound code (a character) or inbound FCode
    name: str               # the character for outbound codes, WinNC's label for FCodes
    fcode: bool             # True when WinNC printed "FCode=" rather than "Code="
    payload: bytes = field(default=b"", repr=False)

    @property
    def overhead(self):
        """L minus the payload bytes shown: the framing WinNC counts in L."""
        return self.length - len(self.payload)

    def to_json(self):
        d = asdict(self)
        d["payload"] = self.payload.hex()
        return d


class LogError(ValueError):
    pass


def parse(text):
    """Every message in a recording, in file order. Raises LogError on a line
    it doesn't understand, with its line number, rather than skipping it."""
    messages = []
    cur = None
    hexbuf = None
    lines = text.splitlines()
    for i, line in enumerate(lines, 1):
        if not line.strip() or line.startswith("==="):
            continue
        m = _HEADER.match(line)
        if m:
            if cur is not None:
                _finish(cur, hexbuf, messages)
            cur = {"dir": "out" if m["dir"] == "O" else "in",
                   "nr": int(m["nr"]), "t": float(m["t"]), "line": i}
            hexbuf = None
            continue
        if cur is None:
            raise LogError(f"line {i}: text before the first message header: {line!r}")
        if hexbuf is None:
            m = _FIELDS.match(line)
            if not m:
                raise LogError(f"line {i}: expected the L=/Z=/Q= line: {line!r}")
            cur["fields"] = m
            hexbuf = bytearray()
            continue
        m = _HEX.match(line)
        if not m:
            raise LogError(f"line {i}: expected hex payload: {line!r}")
        hexbuf += bytes.fromhex(m[1].replace("  ", " "))
    if cur is not None:
        _finish(cur, hexbuf, messages)
    return messages


def _finish(cur, hexbuf, out):
    f = cur.get("fields")
    if f is None:
        raise LogError(f"line {cur['line']}: message header with no L=/Z=/Q= line")
    is_f = f["fcode"] is not None
    out.append(Message(
        direction=cur["dir"], nr=cur["nr"], time=cur["t"],
        length=int(f["l"]), dest=int(f["z"]), src=int(f["q"]),
        code=int(f["fcode"] if is_f else f["code"]),
        name=(f["name"] if is_f else f["ch"]) or "",
        fcode=is_f, payload=bytes(hexbuf or b"")))


def addr(n):
    return ADDRESSES.get(n, str(n))


def code_table(messages):
    """One row per (direction, code), in order of first appearance."""
    rows = OrderedDict()
    for m in messages:
        key = (m.direction, m.code)
        r = rows.get(key)
        if r is None:
            r = rows[key] = {
                "direction": m.direction, "code": m.code, "fcode": m.fcode,
                "name": m.name, "count": 0, "routes": set(), "payload_lengths": set(),
                "overheads": set(), "first": m.time, "example": m.payload}
        r["count"] += 1
        r["routes"].add((m.src, m.dest))
        r["payload_lengths"].add(len(m.payload))
        r["overheads"].add(m.overhead)
        if m.name and not r["name"]:
            r["name"] = m.name
    return list(rows.values())


def _span(values):
    v = sorted(values)
    if len(v) == 1:
        return str(v[0])
    if v == list(range(v[0], v[-1] + 1)):
        return f"{v[0]}–{v[-1]}"
    return ", ".join(map(str, v))


def render_table(rows):
    out = ["| Dir | Code | Name | Count | Route (src→dst) | Payload bytes | L − payload | First (s) | Example |",
           "|---|---|---|---|---|---|---|---|---|"]
    for r in rows:
        code = str(r["code"]) if r["fcode"] else f"{r['code']} `{r['name']}`"
        name = r["name"] if r["fcode"] else ""
        routes = ", ".join(f"{addr(s)}→{addr(d)}" for s, d in sorted(r["routes"]))
        ex = r["example"][:8].hex(" ").upper() + (" …" if len(r["example"]) > 8 else "")
        out.append(f"| {r['direction']} | {code} | {name} | {r['count']} | {routes} | "
                   f"{_span(r['payload_lengths'])} | {_span(r['overheads'])} | "
                   f"{r['first']:.3f} | {ex} |")
    return "\n".join(out)


def render_dump(messages):
    out = []
    for m in messages:
        tag = f"F{m.code} {m.name}" if m.fcode else f"{m.code} '{m.name}'"
        out.append(f"{m.time:9.3f} {m.direction:3} #{m.nr:<4} {addr(m.src):>3}→{addr(m.dest):<3} "
                   f"L={m.length:<3} {tag:28} {m.payload.hex(' ').upper()}")
    return "\n".join(out)


def main(argv=None):
    ap = argparse.ArgumentParser(description=(__doc__ or "").split("\n\n")[0])
    ap.add_argument("log", help="a WinNC RS-485 recording (text)")
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--dump", action="store_true", help="one line per message")
    g.add_argument("--json", action="store_true", help="every message as JSON")
    a = ap.parse_args(argv)
    # The file is from a German Windows machine; its ASCII column has bytes
    # that aren't UTF-8. Only the hex is decoded, so latin-1 loses nothing.
    with open(a.log, encoding="latin-1") as f:
        msgs = parse(f.read())
    if a.json:
        json.dump([m.to_json() for m in msgs], sys.stdout, indent=1)
        print()
    elif a.dump:
        print(render_dump(msgs))
    else:
        print(render_table(code_table(msgs)))
        outs = sum(m.direction == "out" for m in msgs)
        print(f"\n{len(msgs)} messages: {outs} out, {len(msgs) - outs} in, "
              f"{msgs[-1].time if msgs else 0:.3f} s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
