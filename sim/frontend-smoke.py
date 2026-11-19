#!/usr/bin/env python3
"""Smoke test for the emco_lathe screen.

Proves the screen is not merely running but enforcing its central rule:
after an E-stop the machine must be referenced again (docs/40-operator-guide.md
§5). Stock LinuxCNC keeps its homed flags through an E-stop; emco_lathe
un-references the machine as soon as it is switched back on. So if the flags
come back false, the screen is up and its reference gate is working. With
`DISPLAY = axis` this test fails, which is the point.

Run it against a running container (fresh or not):

    docker exec emco-pcturn50-sim python3 /repo/sim/frontend-smoke.py

Exit status 0 means pass.
"""
import linuxcnc, subprocess, sys, time

s = linuxcnc.stat(); c = linuxcnc.command()

def poll():
    s.poll(); return s

def homed():
    poll(); return [bool(s.joint[j]["homed"]) for j in range(s.joints)]

def wait_for(pred, timeout, label):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if pred(): return True
        time.sleep(0.2)
    print("TIMEOUT:", label); return False

# The screen registers a HAL component named after itself once it is up.
def screen_up():
    out = subprocess.run(["halcmd", "list", "comp"], capture_output=True, text=True).stdout
    return "emco_lathe" in out.split()

if not wait_for(screen_up, 60, "emco_lathe HAL component"):
    print("FAIL: the emco_lathe screen is not running (check [DISPLAY]DISPLAY)"); sys.exit(1)
print("screen up")

c.state(linuxcnc.STATE_ESTOP_RESET); c.state(linuxcnc.STATE_ON); c.wait_complete()
c.mode(linuxcnc.MODE_MANUAL); c.wait_complete()
c.home(-1)
if not wait_for(lambda: all(homed()), 90, "homing"):
    sys.exit(1)
print("referenced:", homed())

c.state(linuxcnc.STATE_ESTOP); c.wait_complete(); time.sleep(1)
print("in E-stop, LinuxCNC still says homed:", homed())
c.state(linuxcnc.STATE_ESTOP_RESET); c.state(linuxcnc.STATE_ON); c.wait_complete()

if not wait_for(lambda: not any(homed()), 10, "the screen to un-reference"):
    print("FAIL: still referenced after an E-stop:", homed())
    sys.exit(1)
print("after reset and on:", homed(), "- reference discarded, as on the real machine")
print("SMOKE PASS")
