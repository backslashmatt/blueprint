# 0014: Python in the model

**Status:** accepted (agent decision, 2026-11-23). **Issues:** #153 (analyser), #154 (engine), and the edges issue that follows.

## Context

`lang-python` (#155) reads one file at a time. It gives:
- classes, functions and methods;
- imports and base classes, as references;
- calls, after Python's own scoping rules.

Two things were missing:
- **L1:** Python projects had no systems. `Ecosystem` covered Cargo, npm, Go, .NET, Nx and Bazel, so a Python repository showed only `(unowned)` directories.
- **L2:** the engine had no rule for Python modules.

Python packaging is varied:
- PEP 621 `pyproject.toml` (`[project]`), with any build backend;
- Poetry's own `[tool.poetry]` tables;
- legacy `setup.py`, which is a program, and `setup.cfg`;
- uv workspaces;
- the src layout (`src/pkg/…`) and the flat layout (`pkg/…`).

## Options

**L1, what a project is:**
1. **Any directory with `pyproject.toml`, `setup.cfg` or `setup.py` that declares a project (chosen).**
2. **`pyproject.toml` only.** Simpler, but many existing projects are `setup.py`/`setup.cfg` only, and they would vanish into `(unowned)`.
3. **Run `setup.py` or the build backend for metadata.** Exact, but executing repository code is forbidden (§5.5).

**L2, the module:**
1. **File is the module, named as Python imports it (chosen).**
2. **Package (directory with `__init__.py`) is the module.** This merges the free functions of every file in a package into one component. Python imports and reviews files (`from pkg.io import read`), so that would lose the structure people work with. It is the same reasoning as DR 0012.

## Decision

**1. A system per project directory (`l1/python.rs`).**
- **What counts as a system:**
  - A `pyproject.toml` that declares a project (`[project]`, `[tool.poetry]` with a name, or `[build-system]`) makes its directory a system.
  - So does a `setup.cfg` or `setup.py`.
  - A `pyproject.toml` that only configures tools is not a system. Examples are a repository root with `[tool.ruff]`, or a uv workspace root with `[tool.uv.workspace]`, much as an npm workspace root without a `name` is not one.
- **Name:**
  - `[project].name`, else `[tool.poetry].name`, else `setup.cfg`'s `[metadata] name`, else the directory.
  - A computed name (`attr:`) is not read.
- **Confidence:**
  - `exact` from a parsed `pyproject.toml` that declares the project;
  - `inferred` from `setup.cfg` or `setup.py` alone, because their real metadata can be computed when they run, and `setup.py` is never read;
  - `unknown` when `pyproject.toml` does not parse.
- **Ignored directories:** a hidden directory (`.venv`, `.tox`, `.nox`), `venv`, `site-packages`, `__pycache__` and `node_modules`. They hold installed or generated code, not the repository's own.

**Shared directories.** A Python project often shares its directory with another system: a maturin crate has `Cargo.toml` beside `pyproject.toml`, and a tools `pyproject.toml` can sit beside a `package.json`. Until now, the first system in key order owned every file of a shared directory. Now each file goes to the system whose language it is (`lang::analyser_in`), and only otherwise to the first system. A crate's module tree is read first and may still claim any file of its directory that it reaches with `#[path]` (a `.js` file beside a wasm-pack crate's `package.json`), which keeps the rule that such a file is Rust's. An npm package's L2 pass reads only its own languages' files, so a stray `.py` or `.cs` file in a package is shown by directory, not taken as a module.

**2. Dependency edges, by PEP 503-normalised name.**
- **Sources:** `[project].dependencies` and `[tool.poetry.dependencies]`, without `python`.
- **Not read:** optional, dev and group dependencies, including Poetry dependencies marked `optional = true` (extras), because they do not ship, as with npm's `devDependencies`. An empty name is no name: the directory names the node, and it matches nothing.
- **`exact`:**
  - a uv source or a Poetry dependency with a `path` to a system;
  - a uv `workspace = true` source naming exactly one system.
- **`inferred`:** a plain requirement whose name matches exactly one system in the repository.
- **External, never an edge:**
  - git, URL and index sources;
  - `name @ url` direct references, unless uv sources the name locally.
- **Diagnostics:** a path outside the repository, or a workspace source with no single target.

**3. L2: each `.py` or `.pyi` file is a module (`l2/python.rs`).**
- **Name:** the file's dotted path below the import root, without the extension (`pkg.sub.mod`).
  - `__init__` is its package (`pkg/__init__.py` is `pkg`).
  - An `__init__` directly at the import root is the system itself.
  - Segments are kept in the id as for every language (`L2:python:core#pkg/sub/mod`), and the node's name is dotted.
- **Import root:**
  - `src/` for files under it (the src layout);
  - otherwise the project's directory (the flat layout, `tests/`, scripts).
  - This is what `import` sees once the project is installed, which is how other code names it.
- **Collisions:** when two files would be one module (`x.py` and `x.pyi`, or `src/pkg` beside `pkg`), the first in path order wins, and the others are shown by directory with `unknown` confidence.
- **Confidence and visibility:**
  - modules are `exact`;
  - a module with a `_`-prefixed segment is private, except a dunder such as `__main__`;
  - items follow the analyser's convention (a leading `_` is private, except dunders).
- **L2 shape:** placement depends only on paths, so the resident engine treats a Python file's shape as its path. Body edits apply in place, item edits by item, and structural changes (files added or removed, manifests edited) rebuild.

**4. Edges (#157, `l34/edges/python.rs`).** A Python name resolves in its project's modules, then in those of the Python projects it depends on (its L1 edges), as the longest prefix of the path that is a module; what follows is an item. The standard library, third-party packages and projects it does not declare give no edge.
- **depends-on (L2):**
  - Each `import` gives an edge to the module it resolves to.
  - A relative import climbs from the importing module's package, one package per dot after the first, in its own project only. An `__init__` module is its own package.
  - The edge is `exact` when what follows the module is nothing, `*`, or an item of it, and `inferred` otherwise (a re-export or a variable).
- **extends (L3):** a base class named in the file: a class of its own module, or one it imports (`from m import B`, or `m.B` through `import m`).
- **calls (L4):** through the same names:
  - `f()` and `m.f()` to functions;
  - `C()` to `C.__init__`, and `C.m()` to that method;
  - `self.m()` to a method of the caller's class or, failing that, of a base this file resolves.
  - Calls through other values need types and give none, as in C#.
- **Ambiguous imports:** the analyser writes `import a.b` and `from a import b` alike (`["a", "b"]`), so both readings bind their name (`a` and `b`). Code that uses the reading not meant fails with a `NameError`, so the extra binding resolves nothing real.
- **Incremental:** an item edit re-resolves every Python file that mentions a changed name, as for C#. A method that comes or goes also reaches the files that mention its class, because `C()` and `self.m()` reach methods through the class.

**5. Index keys.** `ANALYSER_VERSION` went to 5 with the engine slice (#154), because Python systems are new nodes for trees that hold Python files. Stored L1–L4 models are keyed by it too, so it goes to 6 with Python edges (#157): a model stored by version 5 has none.

## Measured

- The reference repositories are all Rust, so their benchmarks see no Python files.
- What these benchmarks do measure is the cost of checking every path for a Python manifest. That cost is in the CI bench rows of the PR that adds this record.

## Revisit when

- Namespace packages spread over several projects need to be one module.
- Projects declare their package directory explicitly (`[tool.setuptools.packages.find] where`, `[tool.hatch.build] packages`) in ways that the `src/` rule gets wrong.
- A large repository with `setup.py` packages shows names that only running it would give.
