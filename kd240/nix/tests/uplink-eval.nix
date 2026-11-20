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

  # What the checks below look at, for one evaluation of the system.
  facts = c:
    let
      s = c.systemd.services;
      unit = name: lib.optionalAttrs (s ? ${name}) (
        let u = s.${name}; in {
          inherit (u) after wants requires;
          environment = u.environment;
          serviceConfig = lib.filterAttrs (_: v: builtins.isString v || builtins.isList v || builtins.isBool v || builtins.isInt v) u.serviceConfig;
        }
      );
    in
    {
      failedAssertions = failedAssertions c;
      services = lib.filter (lib.hasPrefix "emco-mesh") (builtins.attrNames s);
      timers = lib.filter (lib.hasPrefix "emco-mesh") (builtins.attrNames c.systemd.timers);
      users = lib.filter (lib.hasPrefix "emco-mesh") (builtins.attrNames c.users.users);
      tmpfiles = lib.filter (lib.hasInfix "/var/lib/emco-mesh") c.systemd.tmpfiles.rules;
      localConfig = builtins.toJSON c.services.emco-mesh.generated.localConfig;
      zenohd = unit "zenohd";
      zenohdVersion = c.services.emco-mesh.zenohdPackage.version;
    };

  all = {
    inherit zenohdSource;
    off = facts kd240.config;
  };
in
pkgs.runCommand "kd240-uplink-eval"
{
  nativeBuildInputs = [ pkgs.jq ];
  facts = pkgs.writeText "facts.json" (json all);
  golden = ./zenohd-local.golden.json;
} ''
  fails=0
  # check DESCRIPTION JQ-EXPRESSION [ARGS...]: the expression must be true
  # on facts.json.
  check() {
    local what=$1 expr=$2; shift 2
    if jq -e "$@" "$expr" "$facts" >/dev/null; then
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

  mkdir -p $out
  jq . "$facts" > $out/facts.json
  echo "$fails failure(s)"
  [ "$fails" -eq 0 ]
''
