# Single source of truth for the daemon's Python dependencies, shared by the devShell and the package.
{
  runtime =
    ps: with ps; [
      mlx-bin
      mlx-lm
      transformers
      numba
      huggingface-hub
      tqdm
    ];

  test =
    ps: with ps; [
      pytest
      pytest-asyncio
    ];
}
