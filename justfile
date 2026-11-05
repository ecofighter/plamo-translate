# Filled in by later tasks.
test:
    @echo "no tests yet"

# Filled in by later tasks.
test-slow:
    @echo "no slow tests yet"

bump-mlx:
    python3 scripts/bump_mlx.py && nix flake check && just test-slow
