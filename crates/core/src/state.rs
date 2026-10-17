// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! The states of a transfer, its named conditions, and the transition table of SPEC §5.4
//! (amendment 0009), as data (`F-003`, T-003-01).
//!
//! The store enforces the same table (`transfer_edge()` and `transitions_apply_lifecycle()` in
//! migration 0014); the core service's tests prove the two agree on every pair (`REQ-TR-6`).

use core::fmt;

/// The states of SPEC §5.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    /// Taken in durably, not yet decided.
    Received,
    /// Refused at request with a named error. Terminal.
    Refused,
    /// Within route rules; the source leg is not yet final.
    Accepted,
    /// Its source leg is final and consumed for it.
    SourceFinal,
    /// A destination transaction is signed and recorded.
    Delivering,
    /// The destination leg is included, not final.
    Delivered,
    /// The destination leg is final and executed in full. Terminal.
    Completed,
    /// The destination leg is proven not executed at finality; a refund transfer exists.
    Refunding,
    /// Its refund transfer is `Completed`. Terminal.
    Refunded,
    /// The source leg never arrived before the request's expiry. Terminal.
    Expired,
    /// The source observation no longer matches the chain, before finality. Terminal.
    Withdrawn,
    /// A leg unknown past its deadline, a partial or mismatched execution, or a condition that
    /// blocks delivery past its deadline.
    NeedsReview,
}

impl State {
    /// Every state, in the order of §5.4.
    pub const ALL: [State; 12] = [
        State::Received,
        State::Refused,
        State::Accepted,
        State::SourceFinal,
        State::Delivering,
        State::Delivered,
        State::Completed,
        State::Refunding,
        State::Refunded,
        State::Expired,
        State::Withdrawn,
        State::NeedsReview,
    ];

    /// The state's name, as the spec and the store write it.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Received => "Received",
            State::Refused => "Refused",
            State::Accepted => "Accepted",
            State::SourceFinal => "Source-Final",
            State::Delivering => "Delivering",
            State::Delivered => "Delivered",
            State::Completed => "Completed",
            State::Refunding => "Refunding",
            State::Refunded => "Refunded",
            State::Expired => "Expired",
            State::Withdrawn => "Withdrawn",
            State::NeedsReview => "Needs-Review",
        }
    }

    /// The state a name stands for.
    pub fn parse(name: &str) -> Option<State> {
        State::ALL.into_iter().find(|s| s.as_str() == name)
    }

    /// Whether the state is terminal: it never changes (`INV-5`).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            State::Refused | State::Completed | State::Refunded | State::Expired | State::Withdrawn
        )
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The named conditions of SPEC §10 (`CND-AwaitingInstruction` is withdrawn by amendment 0007;
/// `CND-CutOff` is amendment 0010's; `CND-PayoutWindow` amendment 0012's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Condition {
    /// A final arrival not bound by a deposit request, or one that cannot become a deposit.
    Unassociated,
    /// The asset's reserve cannot cover a withdrawal.
    ReserveShort,
    /// A fee position cannot pay for the next transaction.
    FeeShort,
    /// The mint window is spent.
    MintWindow,
    /// The issuing network refused a mint because of its destination.
    DestinationRefused,
    /// A delivery moved a different amount.
    AmountMismatch,
    /// An observation stopped matching the chain before finality.
    Withdrawn,
    /// New work for the route waits on a pause.
    Paused,
    /// A convertible-token arrival waiting for its conversion.
    AwaitingConversion,
    /// The transfer's next step falls inside its route's declared cut-off.
    CutOff,
    /// The reserve's outflow allowance for the current window is spent; the withdrawal waits.
    PayoutWindow,
}

impl Condition {
    /// Every condition.
    pub const ALL: [Condition; 11] = [
        Condition::Unassociated,
        Condition::ReserveShort,
        Condition::FeeShort,
        Condition::MintWindow,
        Condition::DestinationRefused,
        Condition::AmountMismatch,
        Condition::Withdrawn,
        Condition::Paused,
        Condition::AwaitingConversion,
        Condition::CutOff,
        Condition::PayoutWindow,
    ];

    /// The condition's id, as the spec and the store write it.
    pub fn as_str(self) -> &'static str {
        match self {
            Condition::Unassociated => "CND-Unassociated",
            Condition::ReserveShort => "CND-ReserveShort",
            Condition::FeeShort => "CND-FeeShort",
            Condition::MintWindow => "CND-MintWindow",
            Condition::DestinationRefused => "CND-DestinationRefused",
            Condition::AmountMismatch => "CND-AmountMismatch",
            Condition::Withdrawn => "CND-Withdrawn",
            Condition::Paused => "CND-Paused",
            Condition::AwaitingConversion => "CND-AwaitingConversion",
            Condition::CutOff => "CND-CutOff",
            Condition::PayoutWindow => "CND-PayoutWindow",
        }
    }

    /// The condition an id stands for.
    pub fn parse(id: &str) -> Option<Condition> {
        Condition::ALL.into_iter().find(|c| c.as_str() == id)
    }

    /// Whether, held in `Source-Final` past its deadline, it sends the transfer to review
    /// (§5.4: `CND-AwaitingConversion`, `CND-DestinationRefused`). Every other condition resumes by
    /// itself and never does.
    pub fn blocks_delivery(self) -> bool {
        matches!(
            self,
            Condition::AwaitingConversion | Condition::DestinationRefused
        )
    }
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An ordinary edge of the §5.4 table: `Some(true)` when it rests on chain data and must record
/// its coordinate (`REQ-TR-2`), `Some(false)` when it need not, `None` when it is not an edge.
///
/// `Delivered -> Delivering` is a new attempt only, and so is `Delivering -> Delivering` when it
/// carries an attempt number; otherwise `Delivering -> Delivering`, like every `S -> S` of a
/// non-terminal state, takes or clears a condition. `Needs-Review -> X` is not an ordinary edge:
/// see [`review_exit`].
pub fn edge(from: State, to: State) -> Option<bool> {
    use State::*;
    match (from, to) {
        (a, b) if a == b && !a.is_terminal() => Some(false),
        (Received, Refused) | (Received, Accepted) => Some(false),
        (Accepted, SourceFinal) | (Accepted, Expired) | (Accepted, Withdrawn) => Some(true),
        (Accepted, NeedsReview) => Some(false),
        (SourceFinal, Delivering) | (SourceFinal, NeedsReview) => Some(false),
        (Delivering, Delivered) | (Delivered, Delivering) => Some(true),
        (Delivering, Refunding) | (Delivered, Refunding) => Some(true),
        (Delivering, NeedsReview) | (Delivered, NeedsReview) => Some(false),
        (Delivered, Completed) | (Refunding, Refunded) => Some(true),
        _ => None,
    }
}

/// Whether a review may end in `to`, given `before`, the state held before review: one ordinary
/// edge from it, never itself and never review again, except a new attempt
/// (`Delivering -> Delivering`), which is an edge of its own (`REQ-RV-2`). Every resolution needs
/// its evidence coordinate.
pub fn review_exit(before: State, to: State, new_attempt: bool) -> bool {
    if to == State::NeedsReview || before == State::NeedsReview {
        return false;
    }
    if to == before {
        return new_attempt && before == State::Delivering;
    }
    edge(before, to).is_some()
}

/// Whether a review resolution into `to` moves value, and so needs a second approver
/// (`REQ-GV-5`): it releases a mint, signs a delivery, or starts a refund.
pub fn resolution_moves_value(to: State) -> bool {
    matches!(
        to,
        State::SourceFinal | State::Delivering | State::Refunding
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_tr_6_names_round_trip() {
        for s in State::ALL {
            assert_eq!(State::parse(s.as_str()), Some(s));
        }
        for c in Condition::ALL {
            assert_eq!(Condition::parse(c.as_str()), Some(c));
        }
        assert_eq!(State::parse("Source Final"), None);
    }

    #[test]
    fn inv_5_no_edge_leaves_a_terminal_state() {
        for from in State::ALL.into_iter().filter(|s| s.is_terminal()) {
            for to in State::ALL {
                assert_eq!(edge(from, to), None, "{from} -> {to}");
            }
        }
    }

    #[test]
    fn req_tr_6_the_two_pairs_that_are_not_edges_on_purpose() {
        assert_eq!(edge(State::Received, State::SourceFinal), None);
        assert_eq!(edge(State::Refunding, State::NeedsReview), None);
    }

    #[test]
    fn req_rv_2_review_ends_one_ordinary_edge_from_the_state_before() {
        assert!(review_exit(State::Accepted, State::SourceFinal, false));
        assert!(review_exit(State::Delivered, State::Completed, false));
        assert!(review_exit(State::Delivering, State::Delivering, true));
        assert!(!review_exit(State::Delivering, State::Delivering, false));
        assert!(!review_exit(State::Accepted, State::Accepted, false));
        assert!(!review_exit(State::Accepted, State::NeedsReview, false));
        assert!(!review_exit(State::Accepted, State::Completed, false));
        assert!(resolution_moves_value(State::Delivering));
        assert!(!resolution_moves_value(State::Completed));
    }
}
