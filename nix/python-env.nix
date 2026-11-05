{
  python3,
  withTests ? false,
}:

let
  deps = import ./python-deps.nix;
in
python3.withPackages (ps: deps.runtime ps ++ (if withTests then deps.test ps else [ ]))
