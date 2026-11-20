# Level 2 of the uplink's checks (W4b): a real zenohd of the board's release
# (this host's build of it) runs the generated uplink config. zenohd rejects
# unknown fields and an ACL policy that names a missing rule at startup, so
# a config that is still running after 5 s (timeout's 124) has a schema
# zenohd accepts. In the sandbox the endpoints cannot connect, and zenohd
# keeps retrying, as it would on the board before the network is up.
#
#   nix build .#checks.x86_64-linux.kd240-uplink-config -L
#
# Its output, uplink.json, is the generated config unchanged
# (kd240/tests/uplink-chain.sh runs it for real).
{ pkgs, kd240, zenohd }:

let
  on = (kd240.extendModules { modules = [{ services.emco-mesh.uplink.enable = true; }]; }).config;
  uplink = builtins.unsafeDiscardStringContext (builtins.toJSON on.services.emco-mesh.generated.uplinkConfig);
in
pkgs.runCommand "kd240-uplink-config"
{
  nativeBuildInputs = [ zenohd pkgs.openssl pkgs.jq ];
  uplink = pkgs.writeText "uplink.json" uplink;
} ''
  fails=0
  # Throwaway TLS files where the config expects ca, cert and key.
  mkdir tls
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 \
    -subj /CN=ca -keyout ca.key -out tls/ca 2>/dev/null
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /CN=kd240-0123456789abcdef \
    -keyout tls/key -out board.csr 2>/dev/null
  openssl x509 -req -in board.csr -CA tls/ca -CAkey ca.key -CAcreateserial -days 2 -out tls/cert 2>/dev/null

  # The config with its files here and endpoints that resolve to nothing.
  jq --arg d "$PWD/tls" '
    .connect.endpoints = ["tcp/127.0.0.1:1", "tls/mesh.invalid:443"]
    | .transport.link.tls.root_ca_certificate = "\($d)/ca"
    | .transport.link.tls.connect_certificate = "\($d)/cert"
    | .transport.link.tls.connect_private_key = "\($d)/key"' "$uplink" > b.json

  # runs CONFIG WHAT: 0 if zenohd was still running after 5 s.
  runs() {
    local rc=0
    RUST_LOG=info timeout 5 zenohd --config "$1" > "$2.log" 2>&1 || rc=$?
    echo "  zenohd on $2: exit $rc"
    [ "$rc" -eq 124 ]
  }
  if runs b.json uplink; then echo "ok    zenohd runs the generated uplink config"
  else echo "FAIL  zenohd exits on the generated uplink config:"; tail -20 uplink.log; fails=$((fails + 1)); fi
  if grep -qi "access control not enabled" uplink.log; then
    echo "FAIL  the ACL was not enabled"; fails=$((fails + 1))
  else echo "ok    the ACL is enabled (no \"not enabled\" in the log)"; fi
  grep -i "access\|acl" uplink.log | grep -v "Initial conf" | head -3 | sed 's/^/        /' || true

  # The controls: the same run must fail on configs zenohd should refuse,
  # or the check above could not fail either.
  jq '.access_control.policies[0].rules += ["no-such-rule"]' b.json > missing-rule.json
  if runs missing-rule.json missing-rule; then echo "FAIL  control: a policy naming a missing rule ran"; fails=$((fails + 1))
  else echo "ok    control: a policy naming a missing rule makes zenohd exit"; fi
  jq '.transport.link.tls.no_such_field = true' b.json > unknown-field.json
  if runs unknown-field.json unknown-field; then echo "FAIL  control: an unknown TLS field ran"; fails=$((fails + 1))
  else echo "ok    control: an unknown TLS field makes zenohd exit"; fi

  mkdir -p $out
  jq . "$uplink" > $out/uplink.json
  cp *.log $out/
  echo "$fails failure(s)"
  [ "$fails" -eq 0 ]
''
