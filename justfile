test:
    cd daemon && pytest -m "not slow"
    cargo test --workspace

test-slow:
    cd daemon && pytest -m slow

bump-mlx:
    python3 scripts/bump_mlx.py && nix flake check && just test-slow
