use std::path::PathBuf;

fn adr_text() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("adr")
        .join("agent-lifecycle.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn stopping_hard_loss_edge_is_explicit() {
    let doc = adr_text();
    assert!(
        doc.contains("Stopping  --> Stopped   : state saved")
            && doc.contains("Stopping  --> Stopped   : hard loss"),
        "state diagram must show distinct graceful and hard-loss Stopping edges"
    );
    assert!(
        doc.contains("while already\n   `Stopping`")
            && doc.contains("hard-loss edge rather than the\n   `state saved` one"),
        "principle 4 must state that a hard loss can strike mid-Stopping"
    );
}

#[test]
fn consequences_record_lock_in_and_reversibility() {
    let doc = adr_text();
    assert!(
        doc.contains("**Lock-in / reversibility.**"),
        "§9 must carry an explicit lock-in / reversibility note"
    );
    let flat = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("Expensive to reverse:"),
        "the note must name which parts are expensive to reverse"
    );
}

#[test]
fn paused_naming_is_deferred_to_the_driver_contract_adr() {
    let doc = adr_text();
    assert!(
        doc.contains("semantics, not the identifier"),
        "the Paused enum/wire naming decision must be recorded as deferred to the RuntimeDriver-contract ADR"
    );
}

#[test]
fn superseded_is_instance_level_and_never_pins_the_instance() {
    let doc = adr_text();
    assert!(
        doc.contains("`superseded` / version-skew is an **instance-level** attribute")
            && doc.contains("never pins an instance"),
        "superseded must stay instance-scoped and preserve the normal lifecycle edges"
    );
}
