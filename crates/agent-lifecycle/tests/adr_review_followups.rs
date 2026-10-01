//! Guard for the ADR-1 review follow-ups (openabdev/studio#3).
//!
//! The wording tightened in `docs/adr/agent-lifecycle.md` during the
//! wontfix-or-fold review pass must stay in place: the distinct hard-loss
//! `Stopping → Stopped` edge, the §9 lock-in/reversibility note, the
//! `State.Paused` wire-naming deferral to the RuntimeDriver-contract ADR, and
//! the instance-level `superseded` phrasing. The doc is read relative to this
//! crate's manifest so the test runs in any checkout layout.

use std::path::Path;

fn adr() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/adr/agent-lifecycle.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// `Stopping → Stopped` edges in the state diagram, as trimmed source lines.
fn stopping_to_stopped_edges(doc: &str) -> Vec<&str> {
    doc.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("Stopping") && l.contains("--> Stopped"))
        .collect()
}

#[test]
fn stopping_has_graceful_and_hard_loss_edges_to_stopped() {
    let doc = adr();
    let edges = stopping_to_stopped_edges(&doc);
    assert!(
        edges.iter().any(|l| l.contains("state saved")),
        "graceful Stopping → Stopped edge (state saved) missing; edges: {edges:?}"
    );
    assert!(
        edges.iter().any(|l| l.contains("hard loss")),
        "hard-loss Stopping → Stopped edge missing — a mid-drain kill must be \
         drawn separately from the graceful 'state saved' edge; edges: {edges:?}"
    );
}

#[test]
fn consequences_record_lock_in_and_reversibility() {
    let doc = adr();
    assert!(
        doc.contains("Lock-in / reversibility"),
        "§9 must name which parts of the 6-state surface are expensive to reverse"
    );
}

#[test]
fn paused_wire_naming_is_deferred_to_the_driver_contract_adr() {
    let doc = adr();
    assert!(
        doc.contains("semantics, not the identifier"),
        "the `Paused` enum/wire naming decision must be recorded as deferred to \
         the RuntimeDriver-contract ADR"
    );
}

#[test]
fn superseded_is_instance_level_and_never_pins_the_instance() {
    let doc = adr();
    assert!(
        doc.contains("never pins an instance"),
        "the superseded note must say the instance keeps its normal edges \
         (→ Unhealthy, → Stopping) rather than being pinned in Paused"
    );
}
