#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use hermes_harness::multiagent::runtime::{Node, Plan, ordered_nodes};

#[test]
fn ownership_overlap_requires_slash_boundary_in_both_directions() {
    for (first, second, overlaps) in [
        ("src", "src", true),
        ("src", "src/lib", true),
        ("src/lib", "src", true),
        ("src", "src-extra", false),
        ("src-extra", "src", false),
        ("src/lib", "src/library", false),
        ("src/library", "src/lib", false),
        ("\u{e9}", "\u{e9}/lib", true),
        ("\u{e9}/lib", "\u{e9}", true),
        ("\u{e9}", "\u{e9}extra", false),
        ("\u{e9}extra", "\u{e9}", false),
        ("src/a", "src/b", false),
        ("日本", "日本/語", true),
        ("日本/語", "日本", true),
        ("日本", "日本語", false),
    ] {
        let plan = Plan::new(
            "run",
            "test",
            vec![
                Node::new("a", "first", &[first]),
                Node::new("b", "second", &[second]),
            ],
        );
        if overlaps {
            assert_eq!(
                ordered_nodes(&plan).unwrap_err().to_string(),
                "Overlapping ownership"
            );
        } else {
            assert!(ordered_nodes(&plan).is_ok());
        }
    }
}
