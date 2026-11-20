"""Level 3 of the uplink's checks (W4b): behaviour cases on a real zenohd chain.

uplink-chain.sh starts, per board, A (the local zenohd, the golden config
on another port) and B (the generated uplink config, pointed at A and at
C), and C, a mesh router with an mTLS listener. This script is the
sessions:

  board<N>  a client of A<N>, as posix-node is: a prefix queryable
            @mw/node/<id>/sys/** answering "<id>:<key>", a subscriber on
            app/unit/** and app/proc/** (what reaches the payloads), and
            the publisher of app/proc/x/state
  local<N>  another client of A<N> (the lathe service, the fleet agent)
  tool      a client of C with the tool cert (mwctl over the mesh)
  observer  a client of C that subscribes to the keys that must not leak

--cloud-acl none runs C with no ACL, so every case tests B's ACL alone;
--cloud-acl on runs it with the mesh router's (W3c's) as well.

  python3 -I uplink_chain.py --case main --certs DIR --a1 tcp/… --a2 tcp/… --c tls/…
"""

import argparse
import json
import sys
import time

import zenoh

N1, N2, N3, N4 = "00000000000000b1", "00000000000000b2", "00000000000000b3", "00000000000000b4"
TIMEOUT = 2.0
# What B lets the mesh query (blueprint section 5), and what it must not.
ALLOWED = ["sys/info", "sys/trust/info", "sys/manifest", "sys/manifest/cm", "sys/payload/binding",
           "sys/payload/start", "sys/payload/stop", "sys/payload/bind", "sys/loader/begin",
           "sys/loader/chunk", "sys/loader/commit"]
DENIED = ["sys/trust/claim", "sys/board", "sys/peers", "sys/fw/begin", "sys/fw/commit", "sys/log",
          "sys/status", "sys/payload/status"]

results = []


def check(name, got, want):
    good = got == want
    results.append(good)
    print(f"{'ok  ' if good else 'BAD '} {name}: got {got}" + ("" if good else f", want {want}"), flush=True)


def session(endpoint, certs=None, cn=None):
    conf = {
        "mode": "client",
        "connect": {"endpoints": [endpoint]},
        "scouting": {"multicast": {"enabled": False}, "gossip": {"enabled": False}},
    }
    if cn:
        conf["transport"] = {"link": {"tls": {
            "root_ca_certificate": f"{certs}/zenoh-ca.crt",
            "connect_certificate": f"{certs}/{cn}.crt",
            "connect_private_key": f"{certs}/{cn}.key",
            "enable_mtls": True,
        }}}
    return zenoh.open(zenoh.Config.from_json5(json.dumps(conf)))


class Board:
    """posix-node as W4a has it: one prefix queryable, concrete reply keys."""

    def __init__(self, sess, node):
        self.sess, self.node = sess, node
        prefix = f"@mw/node/{node}/sys"

        def answer(q):
            key = str(q.key_expr)
            if "*" in key:  # a wildcard gets sys/info, on its concrete key
                key = f"{prefix}/info"
            q.reply(key, f"{node}:{key[len('@mw/node/') + 17:]}")

        self.q = sess.declare_queryable(f"{prefix}/**", answer)
        self.heard = []
        self.subs = [sess.declare_subscriber(k, lambda s: self.heard.append(
            (str(s.key_expr), s.payload.to_string()))) for k in ("app/unit/**", "app/proc/**")]


class Sink:
    def __init__(self, sess, key):
        self.got = []
        self.sub = sess.declare_subscriber(key, lambda s: self.got.append(
            (str(s.key_expr), s.payload.to_string())))


def ask(sess, sel, timeout=TIMEOUT):
    oks = []
    for r in sess.get(sel, target=zenoh.QueryTarget.ALL,
                      consolidation=zenoh.ConsolidationMode.NONE, timeout=timeout):
        if r.ok is not None:
            oks.append(r.ok.payload.to_string())
        else:
            oks.append("ERR " + r.err.payload.to_string())
    return sorted(oks)


def until(fn, want, seconds):
    """fn() until it returns want, for up to seconds; the last value."""
    deadline = time.monotonic() + seconds
    got = fn()
    while got != want and time.monotonic() < deadline:
        time.sleep(1)
        got = fn()
    return got


def case_main(a):
    b1 = session(a.a1)
    b2 = session(a.a2)
    local1 = session(a.a1)
    local2 = session(a.a2)
    tool = session(a.c, a.certs, "tool-t1")
    obs = session(a.c, a.certs, "observer-t1")
    board1, board2 = Board(b1, N1), Board(b2, N2)
    leaks = [Sink(obs, k) for k in ("app/unit/x/cm", "app/telemetry/x", "app/proc/x/state")]
    tool_state = Sink(tool, "app/proc/x/state")
    # Local declarations the mesh must never learn of.
    secrets = [local1.declare_queryable(k, lambda q: q.reply(q.key_expr, "secret"))
               for k in ("@mw/local/secret", "app/unit/x/q", "app/proc/x/q")]
    time.sleep(2.0)

    # B was started before C: the first answer proves its retry from start.
    check("M1 tool @mw/node/b1/sys/info (through C, B, A)",
          until(lambda: ask(tool, f"@mw/node/{N1}/sys/info"), [f"{N1}:sys/info"], 30), [f"{N1}:sys/info"])
    check("M2 tool @mw/node/*/sys/info: both boards", ask(tool, "@mw/node/*/sys/info"),
          sorted([f"{N1}:sys/info", f"{N2}:sys/info"]))
    for k in ALLOWED:
        check(f"M3 tool {k}", ask(tool, f"@mw/node/{N1}/{k}"), [f"{N1}:{k}"])
    for k in DENIED:
        check(f"M4 tool {k} (not exported)", ask(tool, f"@mw/node/{N1}/{k}"), [])
    check("M5 tool @mw/node/** (wide)", ask(tool, "@mw/node/**"), [])
    check("M11 tool @mw/local/secret (a local queryable)", ask(tool, "@mw/local/secret"), [])
    check("M11 tool app/unit/x/q (a local queryable)", ask(tool, "app/unit/x/q"), [])
    check("M11 tool app/proc/x/q (a local queryable)", ask(tool, "app/proc/x/q"), [])
    check("M12 board 2's local client asks board 1", ask(local2, f"@mw/node/{N1}/sys/info"), [])
    check("M13 local client: board 1's trust/claim (unrestricted)",
          ask(local1, f"@mw/node/{N1}/sys/trust/claim"), [f"{N1}:sys/trust/claim"])
    check("M14 local client: @mw/node/b1/** (unrestricted)", ask(local1, f"@mw/node/{N1}/**"),
          [f"{N1}:sys/info"])

    # Publications.
    b1.put("app/proc/x/state", "state-1")
    b1.put("app/unit/x/cm", "unit-1")
    b1.put("app/telemetry/x", "telemetry-1")
    tool.put("app/unit/x/cm", "cloud-unit")
    tool.put("app/proc/x/cmd", "cloud-cmd")
    tool.put("app/proc/x/state", "cloud-state")
    local1.put("app/unit/x/cm", "local-unit")
    time.sleep(1.5)
    # Only what the board published counts: a session's own puts reach its
    # own subscribers, and with no ACL on C the observer hears the tool.
    board = lambda sink: [g for g in sink.got if not g[1].startswith("cloud-")]
    check("M6 the board's process state reaches the tool", board(tool_state), [("app/proc/x/state", "state-1")])
    check("M7 the board's app/unit put never leaves", board(leaks[0]), [])
    check("M8 the board's telemetry is not exported", board(leaks[1]), [])
    check("M6 the observer sees the board's process state", board(leaks[2]), [("app/proc/x/state", "state-1")])
    from_cloud = [h for h in board1.heard if h[1].startswith("cloud-")]
    check("M9/M10 nothing the mesh publishes reaches the board", from_cloud, [])
    check("M15 a local app/unit put reaches the board (unrestricted)",
          [h for h in board1.heard if h[1] == "local-unit"], [("app/unit/x/cm", "local-unit")])
    del secrets, board1, board2, leaks, tool_state
    for s in (b1, b2, local1, local2, tool, obs):
        s.close()


def case_reconnect(a):
    """After C restarted: B links again, by itself."""
    b1 = session(a.a1)
    board1 = Board(b1, N1)
    tool = session(a.c, a.certs, "tool-t1")
    started = time.monotonic()
    got = until(lambda: ask(tool, f"@mw/node/{N1}/sys/info"), [f"{N1}:sys/info"], 45)
    check(f"R1 after C restarts, B reconnects and the tool is answered ({time.monotonic() - started:.0f} s)",
          got, [f"{N1}:sys/info"])
    del board1
    for s in (b1, tool):
        s.close()


def case_other_ca(a):
    """Board 3's B has a cert from another CA: C refuses it; its A is fine."""
    b3 = session(a.a3)
    local3 = session(a.a3)
    board3 = Board(b3, N3)
    tool = session(a.c, a.certs, "tool-t1")
    time.sleep(3)
    check("O1 a board whose cert is from another CA is unreachable", ask(tool, f"@mw/node/{N3}/sys/info"), [])
    check("O2 its local router still serves its clients", ask(local3, f"@mw/node/{N3}/sys/info"),
          [f"{N3}:sys/info"])
    del board3
    for s in (b3, local3, tool):
        s.close()


def case_no_cert(a):
    """Board 4's B has no cert files: its A is untouched."""
    b4 = session(a.a4)
    local4 = session(a.a4)
    board4 = Board(b4, N4)
    time.sleep(1)
    check("X1 with B's certificate missing, A serves its clients", ask(local4, f"@mw/node/{N4}/sys/info"),
          [f"{N4}:sys/info"])
    del board4
    for s in (b4, local4):
        s.close()


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--case", required=True, choices=["main", "reconnect", "other-ca", "no-cert"])
    p.add_argument("--certs", required=True)
    for n in ("a1", "a2", "a3", "a4", "c"):
        p.add_argument(f"--{n}")
    a = p.parse_args()
    {"main": case_main, "reconnect": case_reconnect, "other-ca": case_other_ca, "no-cert": case_no_cert}[a.case](a)
    print(f"{results.count(True)}/{len(results)} cases as expected", flush=True)
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
