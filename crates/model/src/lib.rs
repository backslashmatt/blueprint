//! The architecture model (REQUIREMENTS §3): levels, nodes, edges and
//! confidence. This crate does no I/O and depends on no other workspace crate.

/// One of the four fixed levels of the model hierarchy (§3.1).
/// Drilling down moves exactly one level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// L1: a deployable or publishable unit (app, service, library).
    System,
    /// L2: a package, namespace or directory with a public surface.
    Module,
    /// L3: a type, or a file-level group of functions.
    Component,
    /// L4: a function or method, and its lines.
    Code,
}

impl Level {
    /// All levels, from the most abstract to the most concrete.
    pub const ALL: [Level; 4] = [Level::System, Level::Module, Level::Component, Level::Code];

    /// The level one step more concrete, or `None` at L4.
    pub fn down(self) -> Option<Level> {
        match self {
            Level::System => Some(Level::Module),
            Level::Module => Some(Level::Component),
            Level::Component => Some(Level::Code),
            Level::Code => None,
        }
    }

    /// The level one step more abstract, or `None` at L1.
    pub fn up(self) -> Option<Level> {
        match self {
            Level::System => None,
            Level::Module => Some(Level::System),
            Level::Component => Some(Level::Module),
            Level::Code => Some(Level::Component),
        }
    }

    /// The level's number as written in the requirements: 1 for L1 to 4 for L4.
    pub fn number(self) -> u8 {
        match self {
            Level::System => 1,
            Level::Module => 2,
            Level::Component => 3,
            Level::Code => 4,
        }
    }
}

/// How sure the analyser is of a node or edge (§3.2). Ordered from least to
/// most certain, so the weakest confidence of a roll-up is its minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    /// Not analysed, for example an unsupported language.
    Unknown,
    /// Derived by a heuristic, such as a name match.
    Inferred,
    /// Resolved by the analyser.
    Exact,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drilling_moves_one_level_and_round_trips() {
        for level in Level::ALL {
            if let Some(d) = level.down() {
                assert_eq!(d.number(), level.number() + 1);
                assert_eq!(d.up(), Some(level));
            }
        }
        assert_eq!(Level::System.up(), None);
        assert_eq!(Level::Code.down(), None);
    }

    #[test]
    fn weakest_confidence_is_the_minimum() {
        let edges = [Confidence::Exact, Confidence::Inferred, Confidence::Exact];
        assert_eq!(edges.iter().min(), Some(&Confidence::Inferred));
        assert!(Confidence::Unknown < Confidence::Inferred);
    }
}
