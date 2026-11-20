"""claim-link: a node's local serial link on the KD240, as a PTY that is always drained.

Runs inside each mw-posix-node unit (kd240/nix/mesh.nix, claimLink), beside
the node, started by the unit's wrapper. In $RUNTIME_DIRECTORY (0700):

  tty   a symlink to the slave of a PTY this process owns: the node's
        MW_POSIX_NODE_LOCAL_SERIAL. It is made inside the unit because the
        node's PrivateDevices hides the host's devices.
  sock  a unix socket (0600), at most one client at a time; a new client
        replaces the old. Only root reaches it: claim.sh runs
        `sudo socat - UNIX-CONNECT:…/sock` over ssh for mwctl on the laptop.

The node writes every publication to its local link as well, waiting up to
200 ms per frame for a write. So with no client attached everything the
node writes is read and dropped, and a slow client is dropped rather than
let the PTY fill. The slave is held open here, raw, so the master never
sees EIO when the node closes it.

No dependencies beyond Python's standard library. Run as
`python3 -I claim-link.py` with RUNTIME_DIRECTORY set (or --dir DIR).
"""

import errno
import os
import pty
import select
import signal
import socket
import sys
import tty

# What may wait for a slow client, or for the node, before it is dropped.
MAX_PENDING = 1 << 20
CHUNK = 1 << 16


def log(msg):
    print(f"claim-link: {msg}", file=sys.stderr, flush=True)


class Link:
    def __init__(self, directory):
        self.dir = directory
        os.chmod(self.dir, 0o700)
        self.master, self.slave = pty.openpty()
        tty.setraw(self.slave)
        os.set_blocking(self.master, False)
        self.tty_path = os.path.join(self.dir, "tty")
        self.sock_path = os.path.join(self.dir, "sock")
        # Atomic: the node never sees a half-made or stale link.
        tmp = os.path.join(self.dir, f".tty.{os.getpid()}")
        os.symlink(os.ttyname(self.slave), tmp)
        os.replace(tmp, self.tty_path)
        if os.path.lexists(self.sock_path):
            os.unlink(self.sock_path)
        old = os.umask(0o177)
        try:
            self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.listener.bind(self.sock_path)
        finally:
            os.umask(old)
        os.chmod(self.sock_path, 0o600)
        self.listener.listen(1)
        self.listener.setblocking(False)
        self.client = None
        self.to_client = bytearray()
        self.to_node = bytearray()
        log(f"{self.tty_path} -> {os.ttyname(self.slave)}, clients on {self.sock_path}")

    def drop_client(self, why):
        if self.client is not None:
            log(f"client gone ({why})")
            self.client.close()
        self.client = None
        self.to_client.clear()
        self.to_node.clear()

    def accept(self):
        try:
            conn, _ = self.listener.accept()
        except BlockingIOError:
            return
        if self.client is not None:
            self.drop_client("replaced by a new client")
        conn.setblocking(False)
        self.client = conn
        log("client attached")

    def from_node(self):
        try:
            data = os.read(self.master, CHUNK)
        except BlockingIOError:
            return
        except OSError as e:
            if e.errno == errno.EIO:  # cannot happen while the slave is held
                return
            raise
        if self.client is None:
            return  # drained: nobody listening
        self.to_client += data
        if len(self.to_client) > MAX_PENDING:
            self.drop_client("too slow")

    def from_client(self):
        try:
            data = self.client.recv(CHUNK)
        except BlockingIOError:
            return
        except OSError as e:
            self.drop_client(str(e))
            return
        if not data:
            self.drop_client("closed")
            return
        self.to_node += data
        if len(self.to_node) > MAX_PENDING:
            # The node is not reading its link.
            self.drop_client("the node is not reading")

    def flush(self):
        if self.client is not None and self.to_client:
            try:
                n = self.client.send(self.to_client)
                del self.to_client[:n]
            except BlockingIOError:
                pass
            except OSError as e:
                self.drop_client(str(e))
        if self.to_node:
            try:
                n = os.write(self.master, self.to_node)
                del self.to_node[:n]
            except BlockingIOError:
                pass

    def run(self):
        while True:
            rd = [self.master, self.listener]
            wr = []
            if self.client is not None:
                rd.append(self.client)
                if self.to_client:
                    wr.append(self.client)
            if self.to_node:
                wr.append(self.master)
            r, w, _ = select.select(rd, wr, [])
            if self.master in r:
                self.from_node()
            if self.listener in r:
                self.accept()
            if self.client is not None and self.client in r:
                self.from_client()
            if w or self.to_client or self.to_node:
                self.flush()

    def close(self):
        for p in (self.sock_path, self.tty_path):
            try:
                os.unlink(p)
            except FileNotFoundError:
                pass


def main():
    directory = os.environ.get("RUNTIME_DIRECTORY", "")
    if len(sys.argv) == 3 and sys.argv[1] == "--dir":
        directory = sys.argv[2]
    elif len(sys.argv) != 1:
        print("usage: claim-link.py [--dir DIR]   (default: $RUNTIME_DIRECTORY)", file=sys.stderr)
        return 2
    # systemd's RuntimeDirectory= may list several, colon-separated.
    directory = directory.split(":")[0]
    if not directory or not os.path.isdir(directory):
        log(f"no directory ({directory!r}): set RUNTIME_DIRECTORY or --dir")
        return 1
    link = Link(directory)

    def stop(signum, _frame):
        link.close()
        sys.exit(0)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        link.run()
    finally:
        link.close()


if __name__ == "__main__":
    sys.exit(main())
