# Yapps 2, the parser generator LinuxCNC's halcompile is built with — and
# imports again at runtime (`yapps.runtime`). Not in nixpkgs.
{ lib, buildPythonPackage, fetchPypi, setuptools }:

buildPythonPackage rec {
  pname = "yapps2";
  version = "2.2.0";
  pyproject = true;

  src = fetchPypi {
    pname = "Yapps2";
    inherit version;
    hash = "sha256-+1hC0XF3q8N34yHtzHNJsbkD5NiDmBeE8n+lvuGrAJE=";
  };

  build-system = [ setuptools ];
  pythonImportsCheck = [ "yapps.runtime" ];

  meta = {
    description = "Yet Another Python Parser System";
    homepage = "https://github.com/mk-fg/yapps";
    license = lib.licenses.mit;
  };
}
