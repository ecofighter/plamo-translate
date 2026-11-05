{
  pkgs,
  craneLib,
  home-manager,
  hmModule,
  packages,
  cargoArgs,
  cargoArtifacts,
}:

let
  hmTest =
    let
      hm = home-manager.lib.homeManagerConfiguration {
        inherit pkgs;
        modules = [
          hmModule
          {
            home = {
              username = "t";
              homeDirectory = "/Users/t";
              stateVersion = "25.11";
            };
            services.plamo-translate = {
              enable = true;
              settings.idle_timeout_secs = 120;
            };
          }
        ];
      };
      inherit (hm) config;
      # Discarding context keeps the check from depending on the daemon/CLI builds;
      # the assertions only look at the store path strings.
      summary = builtins.unsafeDiscardStringContext (
        builtins.toJSON {
          agent = config.launchd.agents.plamo-translate;
          hasDirsActivation = config.home.activation ? plamoTranslateDirs;
          dirsActivation = config.home.activation.plamoTranslateDirs or null;
          failedAssertions = map (a: a.message) (builtins.filter (a: !a.assertion) config.assertions);
        }
      );
    in
    pkgs.runCommand "hm-module-check"
      {
        nativeBuildInputs = [ pkgs.jq ];
        inherit summary;
        passAsFile = [ "summary" ];
        configToml = config.xdg.configFile."plamo-translate/config.toml".source;
      }
      ''
        sock="/Users/t/Library/Application Support/plamo-translate/daemon.sock"
        jq . "$summaryPath"
        cat "$configToml"
        jq -e '.failedAssertions == []' "$summaryPath"
        jq -e --arg sock "$sock" '.agent.config.Sockets.Listeners.SockPathName == $sock' "$summaryPath"
        jq -e '.agent.config.Sockets.Listeners.SockPathMode == 384' "$summaryPath"
        jq -e '.agent.config.ProgramArguments | last | contains("libexec/bin/plamo-translated")' "$summaryPath"
        jq -e '.agent.config.EnvironmentVariables.PLAMO_TRANSLATE_MANAGED_BY == "nix"' "$summaryPath"
        jq -e '.agent.config.Label == "local.plamo-translate.daemon"' "$summaryPath"
        jq -e '.agent.restartTriggers | length == 1' "$summaryPath"
        jq -e '.hasDirsActivation' "$summaryPath"
        grep -qxF 'idle_timeout_secs = 120' "$configToml"
        grep -qxF "socket_path = \"$sock\"" "$configToml"
        touch $out
      '';
in
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

  hm-module = hmTest;
}
