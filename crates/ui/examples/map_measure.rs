//! Measures the map on a real repository (#92): building the map of a
//! diff, and drilling into every view two levels deep, against the §4 drill
//! budget (50 ms, input to painted frame).
//!
//! ```text
//! cargo run --release -p blueprint-ui --example map_measure -- <repo> <base> <head>
//! ```

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use blueprint_engine::commits;
use blueprint_ui::map::Map;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [repo, base, head] = args.as_slice() else {
        return Err("usage: map_measure <repo> <base> <head>".into());
    };
    let repo = Path::new(repo);
    let (index, _) = commits::existing_index(repo);
    let d = commits::diff_commits(repo, base, head, index.as_ref())?;
    let (b, h) = (Arc::new(d.base.0), Arc::new(d.head.0));
    let t = Instant::now();
    let mut map = Map::new(Arc::clone(&b), Arc::clone(&h), &d.diff);
    let built = t.elapsed().as_secs_f64() * 1e3;
    let (mut worst, mut total, mut drills) = (0f64, 0f64, 0usize);
    let mut drill = |map: &mut Map| {
        let t = Instant::now();
        let moved = map.drill_down();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        if moved {
            worst = worst.max(ms);
            total += ms;
            drills += 1;
        }
        moved
    };
    for i in 0..map.view.nodes.len() {
        map.selected = Some(i);
        if drill(&mut map) {
            for j in 0..map.view.nodes.len() {
                map.selected = Some(j);
                if drill(&mut map) {
                    map.drill_up();
                }
            }
            map.drill_up();
        }
    }
    println!(
        "nodes {} + {}, map built in {built:.1} ms; {drills} drills: mean {:.2} ms, worst {worst:.2} ms",
        b.nodes().count(),
        h.nodes().count(),
        total / drills.max(1) as f64
    );
    Ok(())
}
