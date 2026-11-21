"""Stand-ins for the claim smoke test (claim-smoke.sh): a node and an mwctl.

They speak one line each way, not mwctl's protocol: what is under test is
the path between them, which is the real one (claim.sh's socat PTY, socat to
the claim link's socket, claim-link.py, its PTY, the node's end).

  claim_fakes.py node TTY STATE    answer on TTY as an unclaimed node would,
                                   keeping the claim in STATE (a file)
  claim_fakes.py mwctl --port P --timeout MS trust info|claim [--key K|--pkcs11 L]
"""

import os
import select
import sys
import termios
import time
import tty


def open_port(path):
    fd = os.open(path, os.O_RDWR | os.O_NOCTTY)
    if os.isatty(fd):
        tty.setraw(fd)
        termios.tcflush(fd, termios.TCIFLUSH)
    return fd


def read_line(fd, timeout):
    buf = b""
    deadline = time.monotonic() + timeout
    while not buf.endswith(b"\n"):
        left = deadline - time.monotonic()
        if left <= 0:
            return None
        r, _, _ = select.select([fd], [], [], left)
        if r:
            chunk = os.read(fd, 256)
            if not chunk:
                return None
            buf += chunk
    return buf.decode()


def node(path, state):
    fd = open_port(path)
    pending = b""
    while True:
        chunk = os.read(fd, 256)
        if not chunk:
            time.sleep(0.05)
            continue
        pending += chunk
        while b"\n" in pending:
            line, pending = pending.split(b"\n", 1)
            words = line.decode().split()
            claimed = os.path.exists(state)
            if words[:2] == ["trust", "info"]:
                if claimed:
                    with open(state) as f:
                        reply = f"trust:            claimed\\nlast version:     0\\npinned key:       {f.read()} (p256)"
                else:
                    reply = "trust:            unclaimed (any upload is accepted; the first claim pins a key)"
            elif words[:2] == ["trust", "claim"] and not claimed:
                with open(state, "w") as f:
                    f.write(words[2])
                reply = f"claimed: pinned p256 key {words[2]}"
            else:
                reply = "ERR denied"
            os.write(fd, reply.encode() + b"\n")


def mwctl(args):
    port = args[args.index("--port") + 1]
    timeout = int(args[args.index("--timeout") + 1]) / 1000
    rest = args[args.index("--timeout") + 2:]
    if rest[:2] == ["trust", "info"]:
        req = "trust info"
    elif rest[:2] == ["trust", "claim"]:
        req = "trust claim " + (rest[3] if len(rest) > 3 else "?")
    else:
        print(f"fake mwctl: {rest}", file=sys.stderr)
        return 2
    fd = open_port(port)
    os.write(fd, req.encode() + b"\n")
    reply = read_line(fd, timeout)
    if reply is None:
        print("mwctl: timed out", file=sys.stderr)
        return 1
    reply = reply.rstrip("\n").replace("\\n", "\n")
    if reply.startswith("ERR"):
        print(f"mwctl: {reply}", file=sys.stderr)
        return 1
    print(reply)
    return 0


if __name__ == "__main__":
    if sys.argv[1] == "node":
        node(sys.argv[2], sys.argv[3])
    else:
        sys.exit(mwctl(sys.argv[1:]))
