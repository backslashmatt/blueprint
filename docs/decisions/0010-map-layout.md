# 0010: Map layout — layered, deterministic, seeded from the last layout

**Status:** accepted (agent decision, 2026-11-18). **Issues:** #91, before the map view (#92).

## Context

§5.4 requires the map's layout to be **stable**: the same model always lays out the same way, and a small change moves few nodes. DR 0003 chose orthogonal edges drawn as quads. A map view shows the children of one node (or the systems), with the model's edges rolled up to the pairs of siblings they join.

The views the reference repositories give are of two kinds (`crates/ui/examples/layout_measure.rs`):

- **Connected graphs:**
  - medium's systems: 52 nodes, 202 edges;
  - medium's largest L2 view, `ide-assists`: 285 nodes, 520 edges;
  - large's `std`: 780 nodes, 2,393 edges, with many long edges;
  - a generated L3 group in `stdarch`: 3,664 nodes, 3,987 edges.
- **Edge-free piles:** test directories of up to 45,263 nodes (large) and 1,125 (medium), with no edge among them.

## Options considered

1. **A layout crate.**
   - `layout-rs` and `rust-sugiyama` (both MIT) do layered layout, but neither offers seeding from an earlier layout, which stability needs (below).
   - Graphviz-style engines are C, or a process to run.

   Rejected: the hard part is stability, and it would have to be built around them anyway.
2. **Force-directed.** Rejected. It is not deterministic without care, it moves everything a little on any change, and it is slow at thousands of nodes.
3. **Layered (Sugiyama-style), written here, with a seeded mode.** **Chosen.** The code is about 450 lines in `ui::layout`, headless, with no dependency.

## Decision

`blueprint_ui::layout`:

- **`layout(ids, edges, metrics)`** produces a fresh layout:
  - Cycles are broken by a depth-first search in id order.
  - Each node's layer is its longest path to a sink, so a dependency is always drawn below its dependents.
  - Long edges get a virtual node per layer they cross.
  - Rows start in id order and get 8 barycentre sweeps, with ties kept in place.
  - Rows are centred.
  - Nodes with no edge go in a grid of about √n columns, in id order, to the right of the graph.
  - The input order never matters: everything is computed in id order.
- **`relayout(ids, edges, metrics, &seed)`**, where `seed = previous.seed(ids)`, lays out a view that has changed a little:
  - A node keeps its old layer unless a new edge needs it deeper.
  - It keeps its old x in that layer, and each bend of a long edge keeps its x as well.
  - New nodes go into the free place nearest their neighbours, so nothing already placed is pushed.
  - Grid nodes keep their cells, and new ones take the first free cells. The grid moves only if the graph now reaches into it.
- **Edges** are orthogonal polylines: down from the bottom centre of a node, across in the channel below its layer, into the top centre of the next node or bend.
- **Base and head share one layout.** The map lays out the union of the view's nodes and edges in base and head, so switching between them, or drawing the diff overlay, moves nothing at all. `relayout`, seeded from the view's previous layout, is used when the model under an open view changes (a new head, a re-index, and the live model in M6).
- Layouts are computed off the UI thread and cached per view (§4: the UI thread never lays out off-screen content).

## Measured

Release build, `layout_measure` on the pinned medium and large commits, fastest of 5. **Moved** is the share of the view's existing nodes that end up somewhere else after one simulated small change: a new node with an edge, a new edge, or an edge removed.

| View | Nodes | Edges | `layout` | `relayout` | Moved, fresh `layout` | Moved, `relayout` |
|---|---|---|---|---|---|---|
| medium: systems | 52 | 202 | 1.2 ms | 0.7 ms | 56% / 96% / 33% | 0% / 4% / 0% |
| medium: `ide-assists` (largest L2) | 285 | 520 | 1.4 ms | 1.1 ms | 34% / 88% / 99% | 0% / 1% / 0% |
| medium: `parser/test_data` (largest, no edges) | 1,125 | 0 | 0.04 ms | 0.3 ms | 100% / 100% / 0% | 100% / 100% / 0% |
| large: systems | 386 | 1,044 | 8.1 ms | 6.1 ms | 99% / 33% / 98% | 62% / 1% / 0% |
| large: `std` (largest L2) | 780 | 2,393 | 54 ms | 46 ms | 12% / 98% / 96% | 10% / 0% / 0% |
| large: `stdarch` generated functions | 3,664 | 3,987 | 11 ms | 8.1 ms | 90% / 25% / 57% | 12% / 0% / 0% |
| large: `tests/` (largest, no edges) | 45,263 | 0 | 9.7 ms | 30 ms | 100% / 100% / 0% | 100% / 100% / 0% |

What this shows:

- **Fresh layout is not stable on its own.** The barycentre sweeps and centring move most of a view after any change. Seeding is what makes a small change move few nodes: none or almost none for an added or removed edge.
- **A new node with an edge** still moves 10–62% of a large view. The new node joins the graph, and when the graph grows into the grid, the grid shifts sideways.
- **An edge-free view gaining its first edge** moves every node, for the same reason. In both cases the move is one sideways shift of the whole grid, not a reshuffle.
- **Time:**
  - medium's largest L2 view takes 1.4 ms;
  - large's `std` takes 54 ms, over the 50 ms drill budget on its own;
  - a 45k-node pile takes 10–30 ms.

  Caching and computing layouts off the UI thread keep this off the input path, but the first drill into `std` will be late. That is for M5's large-repository budgets.

## Revisit when

- Drilling into a view on the large repository misses the 50 ms budget in the frame-time benchmarks (#95, #32). The likely fix is fewer virtual nodes (bundling long edges) or a cheaper ordering for wide layers.
- Reviewers find the grid shift on a view's first edge disruptive. The alternative is to keep a fixed-width gutter between graph and grid.
- The owner's checks (#96) find the layered look wrong for the map.
