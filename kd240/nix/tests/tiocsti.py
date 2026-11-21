"""tiocsti.py TTY: open TTY as a would-be injector does and push one byte.

Opened without O_NOCTTY, so a process with no controlling tty (a service's
main process, a systemd-run unit) makes it its controlling tty, and then
TIOCSTI pushes a byte into the tty's input queue as if typed. Prints one
line for kd240-claim-link-vm:

  open: EACCES          the tty is not ours to open
  tiocsti: EIO          dev.tty.legacy_tiocsti = 0 (no CAP_SYS_ADMIN)
  tiocsti: EPERM        not our controlling tty
  tiocsti: ok           a byte was injected
"""

import errno
import fcntl
import os
import sys
import termios

try:
    fd = os.open(sys.argv[1], os.O_RDWR)
except OSError as e:
    print(f"open: {errno.errorcode.get(e.errno, e.errno)}")
    sys.exit(0)
try:
    fcntl.ioctl(fd, termios.TIOCSTI, b"\x00")
except OSError as e:
    print(f"tiocsti: {errno.errorcode.get(e.errno, e.errno)}")
else:
    print("tiocsti: ok")
