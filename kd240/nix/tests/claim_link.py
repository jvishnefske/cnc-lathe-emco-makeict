"""Tests for kd240/scripts/claim-link.py, the claim link's drain-always PTY daemon (W4b).

Each mw-posix-node on the KD240 gets MW_POSIX_NODE_LOCAL_SERIAL pointing at
a PTY that this daemon makes, as root, in a unit of its own; the PTY's slave
is handed to the node's uid (--owner). The daemon forwards between the PTY
and a unix socket that only its own uid (root) may use, checked by the
peer's credentials and not only by the socket's mode, and while nobody is
attached it reads and discards everything, so the node's writes (every
publication goes to the local link too) never block.

The "node" here opens the PTY's slave the way a program opens a serial
port. Run from anywhere:

    python3 kd240/nix/tests/claim_link.py kd240/scripts/claim-link.py
    nix build .#checks.x86_64-linux.kd240-claim-link -L
"""

import os
import pwd
import select
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import termios
import threading
import time
import unittest

DAEMON = None  # set from argv in main()
WAIT = 5.0


def start_daemon(directory, *extra):
    """The daemon on directory, the slave handed to this test's own user."""
    me = pwd.getpwuid(os.getuid()).pw_name
    return subprocess.Popen([sys.executable, "-I", DAEMON, "--dir", directory, "--owner", me, *extra],
                            stderr=subprocess.PIPE, text=True)


def wait_for(cond, timeout=WAIT, what="condition"):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if cond():
            return
        time.sleep(0.02)
    raise AssertionError(f"timed out waiting for {what}")


def read_exactly(fd_or_sock, n, timeout=WAIT):
    """n bytes from an fd or a socket, or fail after timeout."""
    got = b""
    deadline = time.monotonic() + timeout
    while len(got) < n:
        left = deadline - time.monotonic()
        if left <= 0:
            raise AssertionError(f"read {len(got)} of {n} bytes: {got!r}")
        r, _, _ = select.select([fd_or_sock], [], [], left)
        if not r:
            continue
        if isinstance(fd_or_sock, socket.socket):
            chunk = fd_or_sock.recv(n - len(got))
        else:
            chunk = os.read(fd_or_sock, n - len(got))
        if not chunk:
            raise AssertionError(f"EOF after {len(got)} of {n} bytes")
        got += chunk
    return got


def writes_within(fd, data, timeout):
    """True if a blocking write of all of data to fd returns within timeout.

    The write runs on a thread, so a PTY that is not drained makes this
    False instead of hanging the test."""
    done = threading.Event()

    def write():
        view = memoryview(data)
        try:
            while view:
                view = view[os.write(fd, view):]
        except OSError:
            return
        done.set()

    threading.Thread(target=write, daemon=True).start()
    return done.wait(timeout)


def nothing_within(fd_or_sock, timeout=0.3):
    r, _, _ = select.select([fd_or_sock], [], [], timeout)
    if not r:
        return True
    if isinstance(fd_or_sock, socket.socket):
        return fd_or_sock.recv(4096, socket.MSG_DONTWAIT) == b""
    return False


class ClaimLink(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = os.path.join(self.tmp.name, "emco-mesh-claim-unit")
        os.mkdir(self.dir, 0o777)  # the daemon sets it
        self.proc = start_daemon(self.dir)
        self.tty = os.path.join(self.dir, "tty")
        self.sock = os.path.join(self.dir, "sock")
        wait_for(lambda: os.path.islink(self.tty) and os.path.exists(self.sock), what="tty and sock")
        self.node = None
        self.clients = []

    def tearDown(self):
        for c in self.clients:
            c.close()
        if self.node is not None:
            os.close(self.node)
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(WAIT)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.proc.stderr.close()
        self.tmp.cleanup()

    def open_node(self):
        """The node's end: the slave, opened by its path, as a serial port is."""
        self.node = os.open(self.tty, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        return self.node

    def connect(self):
        c = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        c.connect(self.sock)
        self.clients.append(c)
        return c

    # ---- the files

    def test_tty_is_a_symlink_to_a_pty_slave(self):
        target = os.readlink(self.tty)
        self.assertTrue(target.startswith("/dev/pts/"), target)
        self.assertTrue(stat.S_ISCHR(os.stat(self.tty).st_mode))
        self.assertTrue(os.isatty(self.open_node()))

    def test_socket_0600_and_dir_0755_both_the_daemons(self):
        # The node only follows tty through the dir; nothing in it is the node's.
        st = os.lstat(self.sock)
        self.assertTrue(stat.S_ISSOCK(st.st_mode))
        self.assertEqual(stat.S_IMODE(st.st_mode), 0o600)
        self.assertEqual(st.st_uid, os.geteuid())
        d = os.stat(self.dir)
        self.assertEqual(stat.S_IMODE(d.st_mode), 0o755)
        self.assertEqual(d.st_uid, os.geteuid())
        self.assertEqual(os.lstat(self.tty).st_uid, os.geteuid())

    def test_the_slave_is_the_owners_and_0600(self):
        st = os.stat(self.tty)
        self.assertEqual(st.st_uid, os.getuid())
        self.assertEqual(stat.S_IMODE(st.st_mode), 0o600)

    def test_no_temporary_names_left(self):
        self.assertEqual(sorted(os.listdir(self.dir)), ["sock", "tty"])

    def test_the_slave_is_raw(self):
        attrs = termios.tcgetattr(self.open_node())
        iflag, oflag, lflag = attrs[0], attrs[1], attrs[3]
        self.assertFalse(lflag & (termios.ECHO | termios.ICANON | termios.ISIG))
        self.assertFalse(oflag & termios.OPOST)
        self.assertFalse(iflag & (termios.ICRNL | termios.IXON))

    # ---- draining

    def test_with_no_client_a_megabyte_from_the_node_never_blocks(self):
        node = os.dup(self.open_node())
        os.set_blocking(node, True)
        self.assertTrue(writes_within(node, os.urandom(1 << 20), 10),
                        "the node's writes blocked: the PTY is not drained")

    def test_output_before_a_client_is_not_replayed_to_it(self):
        node = self.open_node()
        os.write(node, b"stale")
        time.sleep(0.3)
        c = self.connect()
        time.sleep(0.2)
        os.write(node, b"fresh")
        self.assertEqual(read_exactly(c, 5), b"fresh")

    # ---- a client

    def test_node_to_client_and_client_to_node(self):
        node = self.open_node()
        c = self.connect()
        time.sleep(0.2)
        os.write(node, b"from the node")
        self.assertEqual(read_exactly(c, 13), b"from the node")
        c.sendall(b"from the client")
        self.assertEqual(read_exactly(node, 15), b"from the client")

    def test_bytes_pass_unchanged_and_nothing_echoes(self):
        node = self.open_node()
        c = self.connect()
        time.sleep(0.2)
        frame = bytes(range(256))  # \n, \r, ^C, ^S, ^D, 0x7f: all of them
        os.write(node, frame)
        self.assertEqual(read_exactly(c, 256), frame)
        c.sendall(frame)
        self.assertEqual(read_exactly(node, 256), frame)
        self.assertTrue(nothing_within(c), "the client's bytes echoed back")

    def test_a_second_client_replaces_the_first(self):
        node = self.open_node()
        first = self.connect()
        time.sleep(0.2)
        second = self.connect()
        time.sleep(0.2)
        first.settimeout(WAIT)
        self.assertEqual(first.recv(16), b"", "the first client was not closed")
        os.write(node, b"to the second")
        self.assertEqual(read_exactly(second, 13), b"to the second")
        second.sendall(b"ok")
        self.assertEqual(read_exactly(node, 2), b"ok")

    def test_a_client_leaving_keeps_the_link_and_the_drain(self):
        node = self.open_node()
        c = self.connect()
        time.sleep(0.2)
        c.close()
        self.clients.remove(c)
        time.sleep(0.2)
        blocking = os.dup(node)
        os.set_blocking(blocking, True)
        self.assertTrue(writes_within(blocking, os.urandom(256 << 10), 10),
                        "not drained after the client left")
        c = self.connect()
        time.sleep(0.2)
        os.write(node, b"again")
        self.assertEqual(read_exactly(c, 5), b"again")

    def test_the_node_reopening_the_tty_still_works(self):
        node = self.open_node()
        os.close(node)
        self.node = None
        node = self.open_node()
        c = self.connect()
        time.sleep(0.2)
        os.write(node, b"reopened")
        self.assertEqual(read_exactly(c, 8), b"reopened")

    # ---- who may connect: the peer's uid, not only the socket's mode

    def test_a_peer_of_another_uid_is_refused(self):
        # The same daemon told that only some other uid may connect: this
        # test's connection (its uid) is closed at once and never joined.
        other = os.path.join(self.tmp.name, "other")
        os.mkdir(other)
        proc = start_daemon(other, "--allow-uid", "4242")
        try:
            sock = os.path.join(other, "sock")
            wait_for(lambda: os.path.exists(sock) and os.path.islink(os.path.join(other, "tty")), what="sock")
            node = os.open(os.path.join(other, "tty"), os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
            c = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            c.connect(sock)
            c.settimeout(WAIT)
            self.assertEqual(c.recv(16), b"", "a refused peer was not closed")
            os.write(node, b"secret")
            time.sleep(0.2)
            with self.assertRaises(OSError):
                c.sendall(b"x" * 65536)  # closed: EPIPE or ECONNRESET
            c.close()
            os.close(node)
        finally:
            proc.terminate()
            proc.wait(WAIT)
            err = proc.stderr.read()
            proc.stderr.close()
        self.assertIn(f"refused a peer with uid {os.getuid()}", err)

    def test_the_daemons_own_uid_may_connect_by_default(self):
        node = self.open_node()
        c = self.connect()
        time.sleep(0.2)
        os.write(node, b"ok")
        self.assertEqual(read_exactly(c, 2), b"ok")

    # ---- stopping

    def test_sigterm_removes_the_socket_and_the_link(self):
        self.proc.send_signal(signal.SIGTERM)
        self.assertEqual(self.proc.wait(WAIT), 0)
        self.assertFalse(os.path.lexists(self.sock))
        self.assertFalse(os.path.lexists(self.tty))


def main():
    global DAEMON
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    DAEMON = os.path.abspath(sys.argv.pop(1))
    unittest.main(verbosity=2)


if __name__ == "__main__":
    main()
