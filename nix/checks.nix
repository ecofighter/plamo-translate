{
  pkgs,
  home-manager,
  hmModule,
  packages,
  rustArgs,
}:

let
  hmTest =
    let
      # Mirrors home-manager's modules/default.nix without its wrapper, which throws on
      # failed assertions before `config` can be inspected.
      hmLib = import "${home-manager}/modules/lib/stdlib-extended.nix" pkgs.lib;
      evalHm =
        {
          homeDirectory ? "/Users/t",
          extra ? { },
        }:
        (hmLib.evalModules {
          class = "homeManager";
          specialArgs.modulesPath = "${home-manager}/modules";
          modules = [
            hmModule
            {
              home = {
                username = "t";
                inherit homeDirectory;
                stateVersion = "25.11";
              };
              services.plamo-translate.enable = true;
            }
            extra
          ]
          ++ import "${home-manager}/modules/modules.nix" {
            inherit pkgs;
            lib = hmLib;
            check = true;
          };
        }).config;
      failedAssertions = config: map (a: a.message) (builtins.filter (a: !a.assertion) config.assertions);
      config = evalHm {
        extra = {
          services.plamo-translate.settings.idle_timeout_secs = 120;
          xdg.configHome = "/Users/t/custom-xdg";
        };
      };
      # Discarding context keeps the check from depending on the daemon/CLI builds;
      # the assertions only look at the store path strings.
      summary = builtins.unsafeDiscardStringContext (
        builtins.toJSON {
          agent = config.launchd.agents.plamo-translate;
          hasDirsActivation = config.home.activation ? plamoTranslateDirs;
          dirsActivation = config.home.activation.plamoTranslateDirs or null;
          failedAssertions = failedAssertions config;
          rejected = {
            idleString = failedAssertions (evalHm {
              extra.services.plamo-translate.settings.idle_timeout_secs = "600";
            });
            idleNegative = failedAssertions (evalHm {
              extra.services.plamo-translate.settings.idle_timeout_secs = -1;
            });
            idleZero = failedAssertions (evalHm {
              extra.services.plamo-translate.settings.idle_timeout_secs = 0;
            });
            # 52-byte home + 56 bytes of "/Library/Application Support/plamo-translate/daemon.sock" = 108 bytes.
            longSocket = failedAssertions (evalHm {
              homeDirectory = "/Users/" + builtins.concatStringsSep "" (builtins.genList (_: "u") 45);
            });
          };
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
        jq -e '.agent.config.EnvironmentVariables.XDG_CONFIG_HOME == "/Users/t/custom-xdg"' "$summaryPath"
        jq -e '.rejected.idleString | any(contains("idle_timeout_secs"))' "$summaryPath"
        jq -e '.rejected.idleNegative | any(contains("idle_timeout_secs"))' "$summaryPath"
        jq -e '.rejected.idleZero == []' "$summaryPath"
        jq -e '.rejected.longSocket | any(contains("104"))' "$summaryPath"
        grep -qxF 'idle_timeout_secs = 120' "$configToml"
        grep -qxF "socket_path = \"$sock\"" "$configToml"
        touch $out
      '';
in
{
  # Debug profile, like `cargo test --workspace` in the justfile: a third of the release compile
  # time, and overflow checks stay on. No build phase: `cargo test` builds the binaries the tests run.
  # The checks ship no binaries, so cargo-auditable has nothing to embed.
  cargo-test = pkgs.rustPlatform.buildRustPackage (
    rustArgs
    // {
      pname = "plamo-translate-cargo-test";
      buildType = "debug";
      auditable = false;
      dontCargoBuild = true;
      cargoTestFlags = [ "--workspace" ];
      installPhase = "touch $out";
    }
  );

  clippy = pkgs.rustPlatform.buildRustPackage (
    rustArgs
    // {
      pname = "plamo-translate-clippy";
      auditable = false;
      nativeBuildInputs = [ pkgs.clippy ];
      buildPhase = ''
        runHook preBuild
        cargo clippy -j "$NIX_BUILD_CORES" --offline --locked --workspace --all-targets -- -D warnings
        runHook postBuild
      '';
      doCheck = false;
      installPhase = "touch $out";
    }
  );

  # pytest (slow tests deselected) runs in the package's checkPhase.
  pytest = packages.plamo-translated;

  hm-module = hmTest;
}
