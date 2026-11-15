# `bp`, the command-line front end

`bp` is the headless front end to the Blueprint engine. Everything the app can show, `bp` can print, so it is what tests and benchmarks drive.

| Command | Effect | Exit code |
|---|---|---|
| `bp --version`, `bp -V` | Prints `bp <version>` | 0 |
| `bp --help`, `bp -h`, `bp` | Prints usage | 0 |
| anything else | Prints an error and usage to stderr | 2 |

Commands for indexing (`bp index`), the model (`bp model`) and diffs (`bp diff`) arrive in milestones M1 and M2.
