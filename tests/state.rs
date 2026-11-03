#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]

use hermes_harness::core::state::{Stage, StateError, create, load, save};

#[test]
fn transitions_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut ticket = create(root, "test-001", "Bounded review").unwrap();
    let err = ticket.advance(Stage::Implement).unwrap_err();
    assert!(matches!(err, StateError::Value(_)));
    assert!(err.to_string().contains("Transition forbidden"));
    let err = ticket.advance(Stage::Facts).unwrap_err();
    assert!(matches!(err, StateError::Value(_)));
    assert!(err.to_string().contains("facts"));
    ticket
        .evidence
        .insert("facts".to_owned(), "facts.md reviewed".to_owned());
    ticket.advance(Stage::Facts).unwrap();
    ticket.advance(Stage::Ticket).unwrap();
    ticket
        .evidence
        .insert("plan".to_owned(), "plan.md".to_owned());
    ticket.advance(Stage::Plan).unwrap();
    let err = ticket.advance(Stage::Implement).unwrap_err();
    assert!(matches!(err, StateError::Permission(_)));
    assert!(err.to_string().contains("plan"));
    ticket.approvals.insert("plan".to_owned(), true);
    ticket.advance(Stage::Implement).unwrap();
    save(root, &ticket).unwrap();
    assert_eq!(load(root, "test-001").unwrap().stage, Stage::Implement);
}

#[test]
fn slug_guard() {
    let dir = tempfile::tempdir().unwrap();
    let err = create(dir.path(), "../escape", "x").unwrap_err();
    assert!(matches!(err, StateError::Value(_)));
    assert!(err.to_string().contains("lower-case slug"));
}

#[test]
fn state_file_key_order_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    create(root, "order-001", "Order").unwrap();
    let path = root.join("tickets").join("order-001").join("state.json");
    let text = concat!(
        "{
",
        "  \"id\": \"order-001\",
",
        "  \"summary\": \"Order\",
",
        "  \"stage\": \"investigate\",
",
        "  \"allowed_paths\": [],
",
        "  \"approvals\": {
",
        "    \"zeta\": true,
",
        "    \"alpha\": false
",
        "  },
",
        "  \"evidence\": {
",
        "    \"plan\": \"p\",
",
        "    \"facts\": \"f\"
",
        "  },
",
        "  \"history\": [
",
        "    {
",
        "      \"to\": \"b\",
",
        "      \"at\": \"a\"
",
        "    }
",
        "  ]
",
        "}
"
    );
    std::fs::write(&path, text).unwrap();
    let ticket = load(root, "order-001").unwrap();
    save(root, &ticket).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn backward_transitions_revoke_approvals_from_target_onward() {
    use hermes_harness::core::state::Ticket;
    for (from, to) in [
        (Stage::Verify, Stage::Implement),
        (Stage::Review, Stage::Plan),
        (Stage::Pr, Stage::Implement),
        (Stage::HumanReview, Stage::Plan),
    ] {
        let mut ticket = Ticket::new("test-rollback", "Rollback");
        ticket.stage = from;
        ticket.evidence.insert("plan".into(), "plan.md".into());
        ticket.approvals.insert("plan".into(), true);
        ticket.approvals.insert("merge".into(), true);
        ticket.advance(to).unwrap();
        assert!(!ticket.approvals.contains_key("plan"));
        assert!(!ticket.approvals.contains_key("merge"));
        if to == Stage::Plan {
            assert!(matches!(
                ticket.advance(Stage::Implement),
                Err(StateError::Permission(_))
            ));
        }
    }
}
