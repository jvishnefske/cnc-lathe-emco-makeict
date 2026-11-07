#!/usr/bin/env python3
"""Acceptance test for the EMCO PC Turn 50 LinuxCNC simulator.

Proves the simulator actually works rather than merely starting: it takes the
machine out of E-stop, references both axes, indexes the turret through an M6,
and runs both example programs to completion, failing on any interpreter error.

Run it against a freshly started container, since it expects an un-referenced
machine:

    docker compose -f sim/compose.yml down
    docker compose -f sim/compose.yml up -d
    docker exec emco-pcturn50-sim python3 /repo/sim/acceptance-test.py

Exit status 0 means pass.
"""
import linuxcnc, os, time, sys

# The example programs, wherever the repository is: /repo in the container,
# the checkout itself when LinuxCNC runs natively (`nix run .#sim-test-native`).
GCODE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "gcode")

s = linuxcnc.stat(); c = linuxcnc.command(); e = linuxcnc.error_channel()

def poll():
    s.poll(); return s

def errs():
    out = []
    while True:
        r = e.poll()
        if not r: break
        out.append(r[1])
    return out

def wait_idle(timeout=300, label=""):
    """Wait for idle, but latch any error seen along the way.

    Checking for errors only until the first idle poll is racy: an error can be
    reported in the same instant the interpreter goes idle, and the run then
    looks like a clean finish. Collect errors and settle before deciding.
    """
    t0 = time.time(); seen = []
    while time.time() - t0 < timeout:
        poll(); seen += errs()
        if s.interp_state == linuxcnc.INTERP_IDLE and s.state != linuxcnc.RCS_EXEC:
            time.sleep(0.5); seen += errs()
            if seen:
                for m in seen: print("  ERROR:", m.strip())
                return False
            return True
        time.sleep(0.2)
    print("  TIMEOUT waiting for idle", label); return False

c.state(linuxcnc.STATE_ESTOP_RESET); c.state(linuxcnc.STATE_ON); c.wait_complete()
poll(); print("machine on:", s.enabled)

c.mode(linuxcnc.MODE_MANUAL); c.wait_complete()
c.home(-1)
t0 = time.time()
while time.time() - t0 < 90:
    poll()
    if all(s.joint[j]["homed"] for j in range(2)): break
    time.sleep(0.2)
poll()
print("homed:", [bool(s.joint[j]["homed"]) for j in range(2)],
      "position:", [round(v,3) for v in s.actual_position[:3:2]])
if not all(s.joint[j]["homed"] for j in range(2)):
    print("FAIL: homing"); sys.exit(1)

# --- M6 turret remap ---
c.mode(linuxcnc.MODE_MDI); c.wait_complete()
print("MDI T3 M6 ...")
c.mdi("T3 M6")
if not wait_idle(60, "toolchange"): print("FAIL: M6 remap"); sys.exit(1)
poll(); print("  tool now:", s.tool_in_spindle)
if s.tool_in_spindle != 3:
    print("FAIL: turret did not index to 3"); sys.exit(1)

# --- run the tutorial part ---
c.mode(linuxcnc.MODE_AUTO); c.wait_complete()
prog = os.path.normpath(os.path.join(GCODE, "tutorial-part.ngc"))
print("running", prog)
c.program_open(prog)
c.auto(linuxcnc.AUTO_RUN, 0)
if not wait_idle(600, "program"): print("FAIL: tutorial-part.ngc"); sys.exit(1)
poll()
print("  finished, tool:", s.tool_in_spindle,
      "pos X%.3f Z%.3f" % (s.actual_position[0], s.actual_position[2]))

# --- run the stepped shaft (subroutines) ---
prog = os.path.normpath(os.path.join(GCODE, "stepped-shaft.ngc"))
print("running", prog)
c.program_open(prog)
c.auto(linuxcnc.AUTO_RUN, 0)
if not wait_idle(600, "program"): print("FAIL: stepped-shaft.ngc"); sys.exit(1)
print("  finished")

print("ACCEPTANCE PASS")
