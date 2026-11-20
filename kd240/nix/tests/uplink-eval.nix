# Level 1 of the uplink's checks (W4b): services.emco-mesh as the real
# kd240 system evaluates it, with the uplink off and on, and the claim
# link. Nothing for aarch64 is built. Every value is read at evaluation
# time and handed to jq as JSON with its store-path context dropped, so
# this is an x86-64 runCommand that takes seconds.
#
#   nix build .#checks.x86_64-linux.kd240-uplink-eval -L
#
# Its output, facts.json, is what was checked (and which zenohd the board
# gets, `zenohdSource`).
{ pkgs, kd240, zenohdSource }:

let
  inherit (pkgs) lib;
  json = v: builtins.unsafeDiscardStringContext (builtins.toJSON v);

  failedAssertions = c: map (a: a.message) (lib.filter (a: !a.assertion) c.assertions);
  # The system with more modules on top.
  extend = modules: (kd240.extendModules { inherit modules; }).config;
  uplinkOn = { services.emco-mesh.uplink.enable = true; };

  # What the checks below look at, for one evaluation of the system.
  facts = c:
    let
      s = c.systemd.services;
      unit = name: lib.optionalAttrs (s ? ${name}) (
        let u = s.${name}; in {
          inherit (u) after wants requires wantedBy bindsTo partOf requisite;
          environment = u.environment;
          serviceConfig = plain u.serviceConfig;
          unitConfig = plain u.unitConfig;
        }
      );
      # Plain values, with a derivation (an ExecStart script) as its path.
      plain = a: lib.filterAttrs (_: v: builtins.isString v || builtins.isList v || builtins.isBool v || builtins.isInt v)
        (lib.mapAttrs (_: v: if lib.isDerivation v then "${v}" else v) a);
      g = c.services.emco-mesh.generated;
    in
    {
      failedAssertions = failedAssertions c;
      services = lib.filter (lib.hasPrefix "emco-mesh") (builtins.attrNames s);
      timers = lib.filter (lib.hasPrefix "emco-mesh") (builtins.attrNames c.systemd.timers);
      users = lib.filter (lib.hasPrefix "emco-mesh") (builtins.attrNames c.users.users);
      tmpfiles = lib.filter (lib.hasInfix "/var/lib/emco-mesh") c.systemd.tmpfiles.rules;
      localConfig = builtins.toJSON g.localConfig;
      uplinkConfig = if g ? uplinkConfig then builtins.toJSON g.uplinkConfig else null;
      zenohd = unit "zenohd";
      uplink = unit "emco-mesh-uplink";
      expiry = unit "emco-mesh-uplink-expiry";
      expiryTimer = lib.optionalAttrs (c.systemd.timers ? emco-mesh-uplink-expiry)
        (plain c.systemd.timers.emco-mesh-uplink-expiry.timerConfig // {
          inherit (c.systemd.timers.emco-mesh-uplink-expiry) wantedBy;
        });
      timeWaitSync = (s.systemd-time-wait-sync or { wantedBy = [ ]; }).wantedBy;
      zenohdVersion = c.services.emco-mesh.zenohdPackage.version;
      socat = lib.any (p: (p.pname or "") == "socat") c.environment.systemPackages;
      enroll = lib.any (p: (p.name or "") == "emco-mesh-enroll") c.environment.systemPackages;
      node = unit "mw-posix-node";
      nodeProcess = unit "mw-posix-node-process";
    };

  all = {
    inherit zenohdSource;
    off = facts kd240.config;
    on = facts (extend [ uplinkOn ]);
    # Each of these must fail an assertion (and only evaluate, never build).
    badEndpoint = failedAssertions (extend [ uplinkOn { services.emco-mesh.uplink.endpoint = "tcp/mesh.updates.vishnefske.com:443"; } ]);
    badTlsDir = failedAssertions (extend [ uplinkOn { services.emco-mesh.uplink.tlsDir = "var/lib/emco-mesh/tls"; } ]);
    oldZenohd = failedAssertions (extend [
      uplinkOn
      { services.emco-mesh.zenohdPackage = lib.mkForce (kd240.pkgs.zenoh // { version = "1.4.0"; }); }
    ]);
    # An old zenohd is fine while the uplink is off (the local router uses
    # nothing new).
    oldZenohdOff = failedAssertions (extend [
      { services.emco-mesh.zenohdPackage = lib.mkForce (kd240.pkgs.zenoh // { version = "1.4.0"; }); }
    ]);
    # The claim link, on and off whatever the board's default is.
    claimOn = facts (extend [{ services.emco-mesh.claimLink.enable = lib.mkForce true; }]);
    claimOff = facts (extend [{ services.emco-mesh.claimLink.enable = lib.mkForce false; }]);
    boardClaimLink = kd240.config.services.emco-mesh.claimLink.enable;
  };
in
pkgs.runCommand "kd240-uplink-eval"
{
  nativeBuildInputs = [ pkgs.jq ];
  facts = pkgs.writeText "facts.json" (json all);
  golden = ./zenohd-local.golden.json;
  uplinkGolden = ./uplink.golden.json;
} ''
  fails=0
  # Key expressions as chunk lists; `*` and `**` intersect anything (even
  # verbatim @ chunks, which they do not in Zenoh: this errs towards FAIL).
  defs='
    def isect(a; b):
      if (a|length) == 0 and (b|length) == 0 then true
      elif (a|length) > 0 and a[0] == "**" then isect(a[1:]; b) or ((b|length) > 0 and isect(a; b[1:]))
      elif (b|length) > 0 and b[0] == "**" then isect(a; b[1:]) or ((a|length) > 0 and isect(a[1:]; b))
      elif (a|length) == 0 or (b|length) == 0 then false
      elif a[0] == "*" or b[0] == "*" or a[0] == b[0] then isect(a[1:]; b[1:])
      else false end;
    def overlaps($k): . as $e | isect($e | split("/"); $k | split("/"));
    def acl: .on.uplinkConfig | fromjson | .access_control;
    # The allow rules of every policy that names the uplink subject.
    def uplinkAllows: acl as $a
      | [$a.policies[] | select(.subjects | index("uplink")) | .rules[]] as $ids
      | [$a.rules[] | select(.permission == "allow" and (.id as $i | $ids | index($i)))];
  '
  # check DESCRIPTION JQ-EXPRESSION [ARGS...]: the expression must be true
  # on facts.json.
  check() {
    local what=$1 expr=$2; shift 2
    if jq -e "$@" "$defs $expr" "$facts" >/dev/null; then
      echo "ok    $what"
    else
      echo "FAIL  $what"; fails=$((fails + 1))
    fi
  }

  echo "zenohd: $(jq -r '.off.zenohdVersion + " from " + .zenohdSource' "$facts")"

  # ---- uplink off: the system as it was
  check "off: no failed assertions" '.off.failedAssertions == []'
  check "off: no emco-mesh-uplink unit" '.off.services | index("emco-mesh-uplink") == null'
  check "off: no expiry timer" '.off.timers == []'
  check "off: no emco-mesh user" '.off.users == []'
  check "off: no tmpfiles under /var/lib/emco-mesh" '.off.tmpfiles == []'
  check "off: the local zenohd's config is the golden one, byte for byte" \
    '.off.localConfig == $g' --rawfile g "$golden"
  check "off: the local zenohd runs that config" \
    '.off.zenohd.serviceConfig.ExecStart | test("/bin/zenohd --config /nix/store/[a-z0-9]{32}-zenohd.json5$")'


  # ---- uplink on: a second zenohd, the local one untouched
  check "on: no failed assertions" '.on.failedAssertions == []'
  check "on: the local zenohd's unit is the same as with the uplink off" '.on.zenohd == .off.zenohd'
  check "on: the local zenohd's config is still the golden one" '.on.localConfig == $g' --rawfile g "$golden"
  check "on: the local zenohd depends on nothing of the uplink's" \
    '[.on.zenohd | (.after, .wants, .requires, .bindsTo, .partOf, .requisite)[] | select(test("uplink"))] == []'
  check "on: the uplink's config is the golden one" \
    '(.on.uplinkConfig | fromjson) == $u[0]' --slurpfile u "$uplinkGolden"
  check "on: the uplink listens on nothing" '(.on.uplinkConfig | fromjson | .listen.endpoints) == []'
  check "on: the uplink connects to the local router, then the mesh" \
    '(.on.uplinkConfig | fromjson | .connect.endpoints) == ["tcp/127.0.0.1:7447", "tls/mesh.updates.vishnefske.com:443"]'
  check "on: a failed connect does not end the uplink" '(.on.uplinkConfig | fromjson | .connect.exit_on_failure) == false'
  check "on: mTLS with ca, cert and key from the TLS dir" \
    '(.on.uplinkConfig | fromjson | .transport.link.tls) == {
       root_ca_certificate: "/var/lib/emco-mesh/tls/ca",
       connect_certificate: "/var/lib/emco-mesh/tls/cert",
       connect_private_key: "/var/lib/emco-mesh/tls/key",
       enable_mtls: true }'
  check "on: ACL enabled, default deny" 'acl | .enabled == true and .default_permission == "deny"'
  check "on: every policy names rules that exist" \
    'acl | ([.rules[].id]) as $r | all(.policies[].rules[]; . as $i | $r | index($i))'
  check "on: every policy names subjects that exist" \
    'acl | ([.subjects[].id]) as $s | all(.policies[].subjects[]; . as $i | $s | index($i))'
  check "on: rule ids are unique" 'acl | ([.rules[].id] | length) == ([.rules[].id] | unique | length)'
  check "on: no uplink allow rule touches app/unit/**" \
    '[uplinkAllows[] | .key_exprs[] | select(overlaps("app/unit/x/cm"))] == []'
  for k in sys/trust/claim sys/fw/begin sys/log sys/board sys/peers sys/status; do
    check "on: the mesh cannot query @mw/node/<id>/$k" \
      '[uplinkAllows[] | select(.messages | index("query")) | .key_exprs[] | select(overlaps("@mw/node/0123456789abcdef/" + $k))] == []' \
      --arg k "$k"
  done
  check "on: the mesh can query @mw/node/<id>/sys/info" \
    '[uplinkAllows[] | select(.messages | index("query")) | .key_exprs[] | select(overlaps("@mw/node/0123456789abcdef/sys/info"))] != []'
  check "on: the uplink unit exists" '.on.services | index("emco-mesh-uplink") != null'
  check "on: it runs zenohd on the uplink's config" \
    '.on.uplink.serviceConfig.ExecStart | test("/bin/zenohd --config /nix/store/[a-z0-9]{32}-zenohd-uplink.json5$")'
  check "on: with the same zenohd as the local router" \
    '(.on.uplink.serviceConfig.ExecStart | split(" ")[0]) == (.on.zenohd.serviceConfig.ExecStart | split(" ")[0])'
  check "on: after the network, the clock and the local router" \
    '.on.uplink.after as $a | all("network-online.target", "time-sync.target", "zenohd.service"; . as $t | $a | index($t))'
  check "on: wants the network and the clock" \
    '.on.uplink.wants as $a | all("network-online.target", "time-sync.target"; . as $t | $a | index($t))'
  check "on: started at boot" '.on.uplink.wantedBy | index("multi-user.target")'
  check "on: ExecStartPre is emco-mesh-enroll check, as root" \
    '.on.uplink.serviceConfig.ExecStartPre | test("^[+]/nix/store/[^ ]+/bin/emco-mesh-enroll check")'
  check "on: restarts on failure, without a start limit" \
    '.on.uplink.serviceConfig.Restart == "on-failure" and .on.uplink.unitConfig.StartLimitIntervalSec == 0'
  check "on: runs as emco-mesh-uplink" \
    '.on.uplink.serviceConfig.User == "emco-mesh-uplink" and .on.uplink.serviceConfig.Group == "emco-mesh-uplink"'
  check "on: hardened like the local router" \
    '.on.uplink.serviceConfig as $u | .on.zenohd.serviceConfig as $z
     | all("NoNewPrivileges", "ProtectSystem", "ProtectHome", "PrivateTmp", "PrivateDevices", "RestrictAddressFamilies", "MemoryMax";
           . as $k | $u[$k] == $z[$k])'
  check "on: systemd-time-wait-sync holds time-sync.target (the board has no RTC)" \
    '.on.timeWaitSync | index("time-sync.target")'
  check "off: and it does not without the uplink" '.off.timeWaitSync | index("time-sync.target") | not'
  check "on: a daily expiry check" '.on.expiryTimer.OnCalendar == "daily" and (.on.expiryTimer.wantedBy | index("timers.target"))'
  check "on: which stops the uplink once the certificate fails" \
    '.on.expiry.serviceConfig.ExecStart | test("/nix/store/[^ ]+emco-mesh-uplink-expiry")'
  check "on: emco-mesh-enroll on the board" '.on.enroll'
  check "off: not without the uplink" '.off.enroll | not'
  check "on: the emco-mesh-uplink user" '.on.users == ["emco-mesh-uplink"]'
  check "on: the TLS dir, root:emco-mesh-uplink 0750" \
    '.on.tmpfiles == ["d /var/lib/emco-mesh 0755 root root -", "d /var/lib/emco-mesh/tls 0750 root emco-mesh-uplink -"]'

  # ---- the claim link: a PTY and its daemon inside each node's unit
  echo "claim link on the board: $(jq -r .boardClaimLink "$facts")"
  check "claim: no failed assertions" '.claimOn.failedAssertions == []'
  for n in node:unit nodeProcess:process; do
    u=''${n%%:*} w=''${n#*:}
    check "claim on: $u's MW_POSIX_NODE_LOCAL_SERIAL is /run/emco-mesh-claim-$w/tty" \
      '.claimOn[$u].environment.MW_POSIX_NODE_LOCAL_SERIAL == "/run/emco-mesh-claim-\($w)/tty"' --arg u "$u" --arg w "$w"
    check "claim on: $u's RuntimeDirectory is emco-mesh-claim-$w, 0700" \
      '.claimOn[$u].serviceConfig | .RuntimeDirectory == "emco-mesh-claim-\($w)" and .RuntimeDirectoryMode == "0700"' \
      --arg u "$u" --arg w "$w"
    check "claim on: $u starts through the claim link wrapper, then the node as before" \
      '.claimOn[$u].serviceConfig.ExecStart as $e | .claimOff[$u].serviceConfig.ExecStart as $plain
       | ($e | test("^/nix/store/[a-z0-9]{32}-emco-mesh-claim-link ")) and ($e | endswith(" " + $plain))' --arg u "$u"
    check "claim on: $u keeps PrivateDevices" '.claimOn[$u].serviceConfig.PrivateDevices == true' --arg u "$u"
    check "claim off: $u has no MW_POSIX_NODE_LOCAL_SERIAL" \
      '.claimOff[$u].environment | has("MW_POSIX_NODE_LOCAL_SERIAL") | not' --arg u "$u"
    check "claim off: $u has no RuntimeDirectory" '.claimOff[$u].serviceConfig | has("RuntimeDirectory") | not' --arg u "$u"
    check "claim off: $u runs the node directly" \
      '.claimOff[$u].serviceConfig.ExecStart | test("^/nix/store/[a-z0-9]{32}-mw-posix-node-[^/]+/bin/mw-posix-node$")' --arg u "$u"
    check "claim on: $u's other settings are as with it off" \
      '[.claimOn, .claimOff] | map(.[$u].serviceConfig | del(.ExecStart, .RuntimeDirectory, .RuntimeDirectoryMode))
       | .[0] == .[1]' --arg u "$u"
  done
  check "claim on: the two nodes have links of their own" \
    '.claimOn.node.serviceConfig.RuntimeDirectory != .claimOn.nodeProcess.serviceConfig.RuntimeDirectory'
  check "claim on: the process node keeps its own state" \
    '.claimOn.nodeProcess.serviceConfig.StateDirectory == "mw-posix-node-process"'
  check "claim: the local zenohd is untouched" '.claimOn.zenohd == .off.zenohd and .claimOff.zenohd == .off.zenohd'
  check "claim on: socat on the board (claim.sh's far end)" '.claimOn.socat'
  check "claim off: and not without it" '.claimOff.socat | not'
  check "claim: the board runs one of the two" \
    'if .boardClaimLink then .off.node == .claimOn.node else .off.node == .claimOff.node end'

  # ---- assertions
  check "a tcp/ endpoint fails an assertion" '.badEndpoint | any(test("uplink.endpoint"))'
  check "a relative TLS dir fails an assertion" '.badTlsDir | any(test("uplink.tlsDir"))'
  check "zenohd 1.4 with the uplink on fails an assertion" '.oldZenohd | any(test("1.10"))'
  check "zenohd 1.4 with the uplink off does not" '.oldZenohdOff == []'

  mkdir -p $out
  jq . "$facts" > $out/facts.json
  echo "$fails failure(s)"
  [ "$fails" -eq 0 ]
''
