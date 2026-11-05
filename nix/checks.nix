{
  craneLib,
  packages,
  cargoArgs,
  cargoArtifacts,
}:

{
  cargo-test = craneLib.cargoTest (cargoArgs // { inherit cargoArtifacts; });

  clippy = craneLib.cargoClippy (
    cargoArgs
    // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
    }
  );

  # pytest (slow tests deselected) runs in the package's checkPhase.
  pytest = packages.plamo-translated;
}
