{ pkgs }:

let
  inherit (pkgs) lib;
  pythonDeps = import ./python-deps.nix;
  pyPkgs = pkgs.python3Packages;

  plamo-translated = pyPkgs.buildPythonApplication {
    pname = "plamo-translated";
    version = "0.1.0";
    pyproject = true;

    # The daemon tests read the shared protocol fixtures from ../protocol/fixtures.
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../daemon
        ../protocol/fixtures
      ];
    };
    sourceRoot = "source/daemon";

    build-system = [ pyPkgs.hatchling ];
    dependencies = pythonDeps.runtime pyPkgs;

    nativeCheckInputs = [ pyPkgs.pytestCheckHook ] ++ pythonDeps.test pyPkgs;
    disabledTestMarks = [ "slow" ];
    pythonImportsCheck = [ "plamo_translated" ];

    meta = {
      mainProgram = "plamo-translated";
      license = with lib.licenses; [
        mit
        asl20
      ];
    };
  };

  # Everything under crates/ is Rust sources, tests, fixtures, or insta snapshots; taking the whole
  # directory keeps new crates and fixture files working without touching this list.
  # Pending insta snapshots (*.snap.new, *.pending-snap) are excluded so a stray one cannot change
  # the build inputs. plamo-client's fixture test reads ../protocol/fixtures relative to its manifest.
  cargoSrc = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      (lib.fileset.difference ../crates (
        lib.fileset.fileFilter (
          f: f.hasExt "new" || f.hasExt "pending-snap" || f.name == ".DS_Store"
        ) ../crates
      ))
      ../protocol/fixtures
    ];
  };

  rustArgs = {
    pname = "plamo-translate";
    version = "0.1.0";
    src = cargoSrc;
    cargoLock.lockFile = ../Cargo.lock;
  };

  plamo-translate = pkgs.rustPlatform.buildRustPackage (
    rustArgs
    // {
      # checks.cargo-test runs the suite; avoid running it twice.
      doCheck = false;
      postInstall = ''
        mkdir -p $out/libexec/bin
        ln -s ${plamo-translated}/bin/plamo-translated $out/libexec/bin/plamo-translated
      '';
      meta = {
        mainProgram = "plamo-translate";
        license = with lib.licenses; [
          mit
          asl20
        ];
      };
    }
  );
in
{
  packages = {
    inherit plamo-translated plamo-translate;
    default = plamo-translate;
  };
  inherit rustArgs;
}
