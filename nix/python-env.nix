{
  python3,
  withTests ? false,
}:

python3.withPackages (
  ps:
  with ps;
  [
    mlx-bin
    mlx-lm
    transformers
    numba
    huggingface-hub
    tqdm
  ]
  ++ (
    if withTests then
      [
        pytest
        pytest-asyncio
      ]
    else
      [ ]
  )
)
