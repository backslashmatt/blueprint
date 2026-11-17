# `bp`, the command-line front end

`bp` is the headless front end to the Blueprint engine. Everything the app can show, `bp` can print, so it is what tests and benchmarks drive.

## Commands

| Command | Effect |
|---|---|
| `bp model <dir>` | Prints the L1–L2 model of a directory as text |
| `bp model <dir> --json` | Prints it as JSON, in the form described in [model-json.md](model-json.md) |
| `bp model <dir> --level 1` | Prints L1 systems and their edges only (`--level 2`, the default, adds L2 modules) |
| `bp model <dir> --rev <rev>` | Prints the model of commit `<rev>` of the git repository at `<dir>`, not of its working tree. `<rev>` is a branch, tag, full or abbreviated commit id, `HEAD`, or any rev-parse expression such as `main~2` |
| `bp index <repo> [--rev <rev>]` | Analyses commit `<rev>` (default `HEAD`) and stores its L1–L2 model in the index, so that `bp model --rev` reopens it without analysing |
| `bp --version`, `bp -V` | Prints `bp <version>` |
| `bp --help`, `bp -h`, `bp help`, `bp` | Prints usage |

`bp model` reads the files under `<dir>` and runs every analyser. It never runs anything from the directory (§5.5), never follows symbolic links, and skips `.git`, `.hg`, `.svn`, `target` and `node_modules`.

With `--rev`, `bp model` reads the commit's files from git's object database through `gix`. It never checks anything out, never runs the `git` binary, hooks or filters, and ignores the working tree. Every file in the commit is read, including any committed `target/` or `node_modules/`. Symbolic links are not followed. Submodules, and paths that are not UTF-8, are reported as warnings.

`bp model --rev` reads the model from the index when the commit's tree is already there, and otherwise analyses the commit without storing it. `bp index` analyses and stores it, then prints `<commit> indexed (<n> nodes)`, or `<commit> already indexed (<n> nodes)`.

The index lives in the platform cache, `~/.cache/blueprint/index/` on Linux. Set `BLUEPRINT_CACHE_DIR` to use another directory. Nothing is ever written inside the repository. The index can be deleted at any time. A damaged index is dropped with a warning and rebuilt, never fatal. Entries are keyed by the commit's tree id, so commits with the same files share one (DR 0007).

Diffs (`bp diff`) arrive in M2.

## Output

- The text form is one line per node, then one per edge, tab-separated. A node line is `id`, `kind`, `name`, `confidence`, then `visibility` if known. An edge line is `from -> to`, `depends-on`, `confidence`, then its dependency kinds.
- The output is deterministic. The same files always print the same bytes, so tests snapshot it.
- Anything an analyser could not fully understand (a malformed manifest, an unresolved module) is printed to stderr as `warning: <path>: <message>`. It does not change the exit code.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success, possibly with warnings |
| 1 | The command could not do its job, for example `<dir>` cannot be read, is not a git repository, `<rev>` does not name a commit, or `bp index` cannot write the index |
| 2 | Usage error: an unknown command, option or value |
