final: prev:

let
  versions = import ./mlx-versions.nix;
in
{
  pythonPackagesExtensions = prev.pythonPackagesExtensions ++ [
    (pyFinal: pyPrev: {
      mlx-metal =
        (pyPrev.mlx-metal.override { macosVariant = "26"; }).overridePythonAttrs
          (_: {
            inherit (versions) version;
            src = pyFinal.fetchPypi {
              pname = "mlx_metal";
              inherit (versions) version;
              format = "wheel";
              dist = "py3";
              python = "py3";
              abi = "none";
              inherit (versions.mlx-metal."26") platform hash;
            };
          });

      mlx-bin = pyPrev.mlx-bin.overridePythonAttrs (_: {
        inherit (versions) version;
        src =
          let
            source = versions.mlx."${pyFinal.python.pythonVersion}-${final.stdenv.hostPlatform.system}";
          in
          pyFinal.fetchPypi {
            pname = "mlx";
            inherit (versions) version;
            inherit (source) platform dist hash;
            format = "wheel";
            python = source.dist;
            abi = source.dist;
          };
      });

      # Packages depending on `mlx` must get the Metal-enabled wheel, never the CPU-only source build.
      mlx = pyFinal.mlx-bin;

      mlx-lm = pyPrev.mlx-lm.overridePythonAttrs (_: {
        inherit (versions.mlx-lm) version;
        src = pyFinal.fetchPypi {
          pname = "mlx_lm";
          inherit (versions.mlx-lm) version hash;
        };
        # The upstream suite needs network access and a Metal device, neither of which the build sandbox has.
        doCheck = false;
      });
    })
  ];
}
