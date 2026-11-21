# The claim link under a real systemd (W4b review): mesh.nix and middleware's
# posix-node module, as on the board, in an x86-64 NixOS VM of the board's
# release (25.11). Only the node is a stand-in: an mw-posix-node that
# answers a line protocol on its MW_POSIX_NODE_LOCAL_SERIAL
# (claim_fakes.py). Everything else is the real unit, user, hardening
# (PrivateDevices, ProtectSystem, SystemCallFilter, no capabilities) and
# the real claim-link.py.
#
# What it shows:
#   - each node, as mw-posix-node inside its PrivateDevices namespace,
#     opens the PTY that its root claim-link unit made (systemd binds the
#     host's /dev/pts into a private /dev and allows char-pts)
#   - root reaches the node through the socket
#   - the node's uid and another user cannot connect at all (mode), and with
#     the socket opened up to everyone the daemon still refuses the node's
#     uid (SO_PEERCRED)
#   - restarting a claim-link unit restarts its node on the new PTY
#
#   nix build .#checks.x86_64-linux.kd240-claim-link-vm -L
{ pkgs, posixNodeModule }:

let
  fakeNode = pkgs.writeShellApplication {
    name = "mw-posix-node";
    runtimeInputs = [ pkgs.python3 ];
    text = ''
      echo "fake mw-posix-node: local link ''${MW_POSIX_NODE_LOCAL_SERIAL:-none}" >&2
      if [ -z "''${MW_POSIX_NODE_LOCAL_SERIAL:-}" ]; then exec sleep infinity; fi
      exec python3 -I ${./claim_fakes.py} node "$MW_POSIX_NODE_LOCAL_SERIAL" "$STATE_DIRECTORY/claimed"
    '';
  };
in
pkgs.testers.runNixOSTest {
  name = "kd240-claim-link-vm";
  nodes.machine = { ... }: {
    # service.nix only for its options (mesh.nix names services.emco-kd240.mesh).
    imports = [ ../mesh.nix ../service.nix posixNodeModule ];
    services.emco-mesh = {
      enable = true;
      posixNodePackage = fakeNode;
      zenohdPackage = pkgs.zenoh;
      driver = "none";
      claimLink.enable = true;
    };
    services.avahi.enable = true;
    users.users.alice = { isNormalUser = true; };
    # An upgraded board: the process node's state as the shared uid left it.
    systemd.tmpfiles.rules = [
      "d /var/lib/mw-posix-node-process 0700 mw-posix-node mw-posix-node -"
      "f /var/lib/mw-posix-node-process/old-trust 0600 mw-posix-node mw-posix-node - from-before"
    ];
    environment.systemPackages = [ pkgs.socat ];
  };
  testScript = ''
    import shlex

    # A request on a claim socket, stdin held open while the answer comes
    # back (the daemon takes a client's EOF as the client leaving, which a
    # bare printf | socat sends at once).
    def ask(sock):
        return (f"(printf 'trust info\\n'; ${pkgs.coreutils}/bin/sleep 2) "
                f"| ${pkgs.socat}/bin/socat -t3 - UNIX-CONNECT:{sock}")

    # As USER, in a transient unit (absolute paths: its PATH is systemd's).
    def as_user(user, cmd):
        return f"systemd-run --wait --pipe -q --uid={user} ${pkgs.bash}/bin/sh -c {shlex.quote(cmd)}"

    machine.wait_for_unit("multi-user.target")
    owner = {"unit": "mw-posix-node", "process": "mw-posix-node-process"}

    def probe(user, tty):
        return machine.succeed(as_user(user, f"${pkgs.python3}/bin/python3 -I ${./tiocsti.py} {tty}")).strip()

    # One uid per node; the process node's old state handed to its own.
    machine.wait_for_unit("mw-posix-node-process.service")
    machine.succeed("test \"$(systemctl show -p User --value mw-posix-node-process)\" = mw-posix-node-process")
    machine.succeed("test \"$(systemctl show -p User --value mw-posix-node)\" = mw-posix-node")
    for f in ("/var/lib/mw-posix-node-process", "/var/lib/mw-posix-node-process/old-trust"):
        machine.succeed(f"test \"$(stat -c %U {f})\" = mw-posix-node-process")

    for which, node in (("unit", "mw-posix-node"), ("process", "mw-posix-node-process")):
        d = f"/run/emco-mesh-claim-{which}"
        machine.wait_for_unit(f"emco-mesh-claim-link-{which}.service")
        machine.wait_for_unit(f"{node}.service")
        machine.wait_until_succeeds(f"journalctl -u {node} | grep -q 'local link {d}/tty'")
        machine.fail(f"journalctl -u {node} | grep -q 'NO CLAIM LINK'")

        # Who owns what.
        machine.succeed(f"test \"$(stat -c '%U %a' {d})\" = 'root 755'")
        machine.succeed(f"test \"$(stat -c '%U %a %F' {d}/sock)\" = 'root 600 socket'")
        machine.succeed(f"test \"$(stat -L -c '%U %a' {d}/tty)\" = '{owner[which]} 600'")
        # The node's process (in its own namespace) has the slave open.
        machine.succeed(f"ls -l /proc/$(systemctl show -p MainPID --value {node})/fd | grep -q /dev/pts/")

        # Root reaches the node, through its root-made PTY.
        out = machine.succeed(ask(f"{d}/sock"))
        assert "unclaimed" in out, out

        # The node's uid and another user cannot connect: the kernel says no.
        for user in ("mw-posix-node", "alice"):
            out = machine.succeed(as_user(user, ask(f"{d}/sock") + " 2>&1; true"))
            assert "Permission denied" in out and "unclaimed" not in out, (user, out)
        # Even with the socket opened to everyone, the daemon refuses them.
        machine.succeed(f"chmod 0666 {d}/sock")
        out = machine.succeed(as_user("mw-posix-node", ask(f"{d}/sock") + " 2>&1; true"))
        assert "unclaimed" not in out and "Permission denied" not in out, out
        uid = machine.succeed("id -u mw-posix-node").strip()
        machine.wait_until_succeeds(
            f"journalctl -u emco-mesh-claim-link-{which} | grep -q 'refused a peer with uid {uid}'",
            timeout=60,
        )
        machine.succeed(f"chmod 0600 {d}/sock")

    # Neither node, nor anyone else, can open the other node's tty: no
    # TIOCSTI into it, no writing to it.
    unit_tty, proc_tty = "/run/emco-mesh-claim-unit/tty", "/run/emco-mesh-claim-process/tty"
    for user, tty in (("mw-posix-node-process", unit_tty), ("mw-posix-node", proc_tty),
                      ("alice", unit_tty), ("alice", proc_tty)):
        out = probe(user, tty)
        assert out == "open: EACCES", (user, tty, out)

    # And on its own tty TIOCSTI is refused, by dev.tty.legacy_tiocsti = 0.
    sysctl = "/proc/sys/dev/tty/legacy_tiocsti"
    machine.succeed(f"test -e {sysctl}")
    assert machine.succeed(f"cat {sysctl}").strip() == "0"
    for user, tty in (("mw-posix-node", unit_tty), ("mw-posix-node-process", proc_tty)):
        out = probe(user, tty)
        assert out == "tiocsti: EIO", (user, tty, out)
    # The control: with the sysctl at 1 the same probe injects (its own
    # tty becomes its controlling tty), so the probe can see it.
    machine.succeed(f"echo 1 > {sysctl}")
    out = probe("mw-posix-node", unit_tty)
    machine.succeed(f"echo 0 > {sysctl}")
    assert out == "tiocsti: ok", out

    # A claim link restarted is a new PTY: its node restarts with it.
    pid = machine.succeed("systemctl show -p MainPID --value mw-posix-node").strip()
    machine.succeed("systemctl restart emco-mesh-claim-link-unit.service")
    machine.wait_for_unit("mw-posix-node.service")
    machine.wait_until_succeeds(
        f"test \"$(systemctl show -p MainPID --value mw-posix-node)\" != {shlex.quote(pid)}"
    )
    machine.wait_until_succeeds(
        ask("/run/emco-mesh-claim-unit/sock") + " | grep -q unclaimed"
    )
  '';
}
