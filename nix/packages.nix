{ pkgs, craneLib }:

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

    meta.mainProgram = "plamo-translated";
  };

  cargoSrc = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      (craneLib.fileset.commonCargoSources ../.)
      ../protocol/fixtures
      ../crates/plamo-translate/tests/fixtures
      ../crates/plamo-translate/src/daemon/snapshots
    ];
  };

  cargoArgs = {
    src = cargoSrc;
    pname = "plamo-translate";
    version = "0.1.0";
    strictDeps = true;
  };

  cargoArtifacts = craneLib.buildDepsOnly cargoArgs;

  plamo-translate = craneLib.buildPackage (
    cargoArgs
    // {
      inherit cargoArtifacts;
      # The cargo-test check runs the suite; avoid running it twice.
      doCheck = false;
      postInstall = ''
        mkdir -p $out/libexec/bin
        ln -s ${plamo-translated}/bin/plamo-translated $out/libexec/bin/plamo-translated
      '';
      meta.mainProgram = "plamo-translate";
    }
  );
in
{
  packages = {
    inherit plamo-translated plamo-translate;
    default = plamo-translate;
  };
  inherit cargoArgs cargoArtifacts;
}
