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
  - `exact` from a parsed `pyproject.toml`;
  - `inferred` from `setup.cfg` or `setup.py` alone, because their real metadata can be computed when they run, and `setup.py` is never read;
  - `unknown` when `pyproject.toml` does not parse.
- **Ignored directories:** a hidden directory (`.venv`, `.tox`, `.nox`), `venv`, `site-packages`, `__pycache__` and `node_modules`. They hold installed or generated code, not the repository's own.

**2. Dependency edges, by PEP 503-normalised name.**
- **Sources:** `[project].dependencies` and `[tool.poetry.dependencies]`, without `python`.
- **Not read:** optional, dev and group dependencies, because they do not ship, as with npm's `devDependencies`.
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
  - a module with a `_`-prefixed segment is private;
  - items follow the analyser's convention (a leading `_` is private, except dunders).
- **L2 shape:** placement depends only on paths, so the resident engine treats a Python file's shape as its path. Body edits apply in place, item edits by item, and structural changes (files added or removed, manifests edited) rebuild.

**4. Edges (imports and bases) come next, in their own slice.**
- Until then, Python files give no L3 or L4 edges.
- A missing edge is better than an invented one (§2.3).

**5. Index keys.** `ANALYSER_VERSION` goes to 5, because Python systems are new nodes for trees that hold Python files.

## Measured

- The reference repositories are all Rust, so their benchmarks see no Python files.
- What these benchmarks do measure is the cost of checking every path for a Python manifest. That cost is in the CI bench rows of the PR that adds this record.

## Revisit when

- Namespace packages spread over several projects need to be one module.
- Projects declare their package directory explicitly (`[tool.setuptools.packages.find] where`, `[tool.hatch.build] packages`) in ways that the `src/` rule gets wrong.
- A large repository with `setup.py` packages shows names that only running it would give.
