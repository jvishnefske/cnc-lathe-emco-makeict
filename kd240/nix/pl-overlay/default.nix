# The emco_pl_overlay kernel module (emco_pl_overlay.c), built against the
# board's cross-compiled kernel. Called through linuxPackagesFor, which
# supplies the kernel and its cross make flags.
{ stdenv, kernel, kernelModuleMakeFlags }:

stdenv.mkDerivation {
  pname = "emco-pl-overlay";
  version = "0.1-${kernel.modDirVersion}";
  src = ./.;
  hardeningDisable = [ "pic" ];
  nativeBuildInputs = kernel.moduleBuildDependencies;
  makeFlags = kernelModuleMakeFlags ++ [
    "-C" "${kernel.dev}/lib/modules/${kernel.modDirVersion}/build"
    "M=$(PWD)"
  ];
  buildFlags = [ "modules" ];
  installPhase = ''
    install -Dm644 emco_pl_overlay.ko \
      $out/lib/modules/${kernel.modDirVersion}/extra/emco_pl_overlay.ko
  '';
}
