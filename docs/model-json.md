# The model's JSON form

`bp model --json` (M1) prints a `Model`. All collections are sorted, so the same model always serialises to the same bytes; snapshot tests rely on this.

```json
{
  "nodes": {
    "L1:engine":          { "id": "L1:engine", "level": "L1", "kind": "cargo-package", "name": "engine",
                            "path": "crates/engine/Cargo.toml", "parent": null, "confidence": "exact" },
    "L2:engine/git/tree": { "id": "L2:engine/git/tree", "level": "L2", "kind": "module", "name": "tree",
                            "path": "crates/engine/src/git/tree.rs", "parent": "L1:engine", "confidence": "exact" }
  },
  "edges": [
    { "from": "L1:cli", "to": "L1:engine", "kind": "depends-on", "confidence": "exact", "evidence": [] }
  ]
}
```

- **`id`**: `L<level>:<system>[/<path within the system>]`. It is derived from location, never content, so a node keeps its id across commits.
- **`level`**: `L1`–`L4` (REQUIREMENTS §3.1).
- **`kind`**: one of `cargo-package`, `npm-package`, `go-module`, `dotnet-project`, `nx-project`, `bazel-package`, `unowned` (L1), `module`, `directory` (L2), `component` (L3) or `function` (L4).
- **`parent`**: the node one level up; `null` only at L1.
- **`confidence`**: `exact`, `inferred` or `unknown` (§3.2).
- **Edge `kind`**: `depends-on`, `calls`, `implements`, `extends`, `reads`, `writes` or `exposes`.
- **Edge `evidence`**: the `[from, to]` pairs of the lower-level edges a rolled-up edge stands for.

Deserialising does not re-check the hierarchy rules that `Model::add_node` and `Model::add_edge` enforce. Only feed it JSON that the engine produced.
