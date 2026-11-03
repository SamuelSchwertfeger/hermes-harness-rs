#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Tests for scoped Cyber Warrior dispatch authorization.

use hermes_harness::multiagent::cyber::{CyberGrant, validate_cyber_node};
use hermes_harness::multiagent::runtime::{Node, Plan, Role, ordered_nodes};

fn paths(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn missing_grant_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let error = validate_cyber_node("audit", &paths(&["src/a.py"]), tmp.path(), None).unwrap_err();
    assert!(error.to_string().contains("explicit operator grant"));
}

#[test]
fn authorization_is_scoped() {
    let tmp = tempfile::tempdir().unwrap();
    let grant = CyberGrant::new(tmp.path(), &["audit"], &["src/a.py"], "operator");
    let a = paths(&["src/a.py"]);
    validate_cyber_node("audit", &a, tmp.path(), Some(&grant)).unwrap();
    let error = validate_cyber_node("other", &a, tmp.path(), Some(&grant)).unwrap_err();
    assert!(error.to_string().contains("task not authorized"));
    let error =
        validate_cyber_node("audit", &paths(&["src/b.py"]), tmp.path(), Some(&grant)).unwrap_err();
    assert!(error.to_string().contains("exceeds grant"));
    let error =
        validate_cyber_node("audit", &a, tmp.path().parent().unwrap(), Some(&grant)).unwrap_err();
    assert!(error.to_string().contains("repository mismatch"));
}

#[test]
fn roles_are_validated() {
    let error = "root".parse::<Role>().unwrap_err();
    assert!(error.to_string().contains("Unknown worker role"));
    let cyber = Plan::new(
        "security",
        "review",
        vec![Node::new("audit", "review code", &["src/a.py"]).with_role(Role::CyberWarrior)],
    );
    assert_eq!(ordered_nodes(&cyber).unwrap()[0].role, Role::CyberWarrior);
}
