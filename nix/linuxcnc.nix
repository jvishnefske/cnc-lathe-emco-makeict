# LinuxCNC 2.9.10, uspace build, from source. Not in nixpkgs.
#
# This is the same version sim/Dockerfile installs from linuxcnc.org's apt
# repository, so the simulator config and the emco_lathe screen behave the
# same either way.
#
# Built for SIMULATION. `rtapi_app` is installed without its usual setuid bit
# (the Nix store does not allow one, and the simulator does not need realtime
# scheduling — sim/Dockerfile drops it for the same reason). Driving real
# hardware wants a realtime kernel and a privileged rtapi_app, which is out of
# scope here: see docs/60-retrofit-design.md.
{ lib
, stdenv
, fetchFromGitHub
, autoconf
, automake
, pkg-config
, gettext
, intltool
, asciidoc-full
, groff
, makeWrapper
, wrapGAppsHook3
, qt5
, python3
, yapps2
, boost
, libepoxy
, libGL
, libGLU
, gtk3
, gobject-introspection
, libmodbus
, libgpiod_1
, libedit
, libtirpc
, libusb1
, xorg
, tcl
, tk
, tclPackages
, procps
, psmisc
, util-linux
, coreutils
, gnugrep
, gnused
, gawk
, which
, mesa-demos
}:

let
  # Everything LinuxCNC's Python imports: AXIS (Tk), gladevcp/hal_glib (GObject),
  # QtVCP (PyQt5), and the G-code backplot (OpenGL). Debian's linuxcnc-uspace
  # Depends: lists the same set.
  pythonEnv = python3.withPackages (ps: with ps; [
    tkinter
    numpy
    pycairo
    pygobject3
    pyopengl
    configobj
    xlib
    pillow
    psutil
    pyqt5
    qscintilla-qt5
    dbus-python
    yapps2
  ]);

  tclLibs = [ tclPackages.bwidget tclPackages.tclx ];

  # milltask and rs274 embed the Python interpreter through Boost.Python,
  # which nixpkgs' default boost is built without.
  boostPython = boost.override { enablePython = true; python = python3; };
in
stdenv.mkDerivation rec {
  pname = "linuxcnc";
  version = "2.9.10";

  src = fetchFromGitHub {
    owner = "LinuxCNC";
    repo = "linuxcnc";
    rev = "v${version}";
    hash = "sha256-q/tuSBJ14bCTax2mnq3R/1gSTKgb6jBpNVbf5oPPy0E=";
  };

  sourceRoot = "${src.name}/src";
  # The build lives in src/ but writes into its siblings (scripts/, bin/,
  # lib/, rtlib/), and Nix only makes sourceRoot itself writable.
  postUnpack = ''
    chmod -R u+w "${src.name}"
  '';

  nativeBuildInputs = [
    autoconf
    automake
    pkg-config
    gettext
    intltool
    asciidoc-full # a2x: man pages are built even with documentation disabled
    groff         # preconv, for the HAL components' man pages
    makeWrapper
    wrapGAppsHook3
    qt5.wrapQtAppsHook
    gobject-introspection
    pythonEnv
    # configure records the absolute paths of these, and LinuxCNC's
    # `realtime` script runs them to start and stop the HAL.
    procps     # ps, kill, pidof
    psmisc     # fuser
    util-linux # ipcs
  ];

  buildInputs = [
    pythonEnv
    boostPython
    libepoxy
    libGL
    libGLU
    gtk3
    libmodbus
    libgpiod_1
    libedit # halcmd and halrun line editing
    libtirpc
    libusb1
    xorg.libXmu
    xorg.libXinerama
    tcl
    tk
    qt5.qtbase
    qt5.qtsvg
  ] ++ tclLibs;

  # Some install directories are absolute, outside any prefix.
  postPatch = ''
    substituteInPlace Makefile \
      --replace-fail '$(DESTDIR)/lib/linuxcnc' '$(DESTDIR)$(libdir)/linuxcnc' \
      --replace-fail '$(DESTDIR)/etc/X11/app-defaults' '$(DESTDIR)$(sysconfdir)/X11/app-defaults' \
      --replace-fail '$(DESTDIR)/usr/share/applications/' '$(DESTDIR)$(datadir)/applications/'
  '';

  preConfigure = ''
    ./autogen.sh
  '';

  configureFlags = [
    "--with-realtime=uspace"
    "--disable-build-documentation"
    "--disable-check-runtime-deps"
    "--disable-gtk2"
    "--with-locale-dir=${placeholder "out"}/share/locale"
    "--with-tclConfig=${tcl}/lib/tclConfig.sh"
    "--with-tkConfig=${tk}/lib/tkConfig.sh"
    "--with-boost-libdir=${boostPython}/lib"
  ];

  enableParallelBuilding = true;
  buildFlags = [ "build-software" ];

  # The Makefile installs as `install -o root`, puts Python modules in Debian's
  # dist-packages and makes rtapi_app setuid. None of that works in the store.
  installFlags = [
    "INSTALL=install"
    "SITEPY=${placeholder "out"}/${python3.sitePackages}"
  ];
  # A value with spaces in it cannot go in installFlags, which is split on them.
  preInstall = ''
    installFlagsArray+=("SETUID=install -m 0755")
  '';
  installTargets = [ "install-software" ];

  # Upstream's sample configs link to nc_files/, which only the documentation
  # install provides. Nothing here uses the samples, so drop the dangling links.
  postInstall = ''
    find $out/share/doc -xtype l -print -delete
  '';

  dontWrapGApps = true;
  dontWrapQtApps = true;

  postFixup = ''
    # patchShebangs resolves `python3` to the bare interpreter, which has none
    # of the modules above. Every Python program here needs the full set.
    for f in $out/bin/*; do
      [ -f "$f" ] && head -c2 "$f" | grep -q '#!' || continue
      sed -i -e '1s|^#!.*/bin/python3[^ ]*|#!${pythonEnv}/bin/python3|' "$f"
    done

    for f in $out/bin/*; do
      [ -f "$f" ] && [ -x "$f" ] || continue
      wrapProgram "$f" \
        "''${gappsWrapperArgs[@]}" \
        "''${qtWrapperArgs[@]}" \
        --prefix PATH : "$out/bin:${lib.makeBinPath [ pythonEnv tcl tk procps psmisc coreutils gnugrep gnused gawk which mesa-demos ]}" \
        --prefix PYTHONPATH : "$out/${python3.sitePackages}" \
        --prefix TCLLIBPATH " " "$out/lib/tcltk ${lib.concatMapStringsSep " " (p: "${p}/lib") tclLibs}"
    done
  '';

  passthru = { inherit pythonEnv; };

  meta = {
    description = "Motion controller for CNC machines (uspace build, for simulation)";
    homepage = "https://linuxcnc.org/";
    license = lib.licenses.gpl2Plus;
    platforms = lib.platforms.linux;
    mainProgram = "linuxcnc";
  };
}
