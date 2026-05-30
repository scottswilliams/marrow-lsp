//! The pure stop decision: given the resume mode the client last issued and the
//! statement the run is about to execute, decide whether to stop here.
//!
//! This is the heart of statement stepping, kept free of threads, channels, and
//! I/O so it can be exhaustively unit-tested. The run-thread consults it once per
//! statement (in `before_statement`); the protocol thread sets the [`Resume`] mode
//! each time it lets the run go.

/// How the run should proceed after a stop, as chosen by the client's last
/// continue/step/pause request. Each variant carries the activation depth at the
/// statement the request was issued from, since step-over and step-out are
/// expressed relative to that depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    /// Run until a breakpoint is hit (`continue`).
    Continue,
    /// Stop at the next statement at the same or a shallower depth (`next` /
    /// step-over): a call made by this statement runs to completion without
    /// stopping inside it.
    Over { from_depth: usize },
    /// Stop at the very next statement, at any depth (`stepIn`).
    Into,
    /// Stop at the next statement strictly shallower than the issuing depth
    /// (`stepOut`): the current activation finishes and we stop in its caller.
    Out { from_depth: usize },
    /// Stop at the next statement, whatever its depth (`pause`). Distinct from
    /// [`Resume::Into`] only in intent; the stop reason differs.
    Pause,
}

/// Why the run stopped, reported on the DAP `stopped` event. The reason a client
/// sees should match the action it took, so `next`/`stepIn`/`stepOut`/`pause`
/// each map to their own reason and a breakpoint always reports `breakpoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Breakpoint,
    Step,
    Pause,
    Entry,
}

impl StopReason {
    /// The DAP `reason` string for a `stopped` event.
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Breakpoint => "breakpoint",
            StopReason::Step => "step",
            StopReason::Pause => "pause",
            StopReason::Entry => "entry",
        }
    }
}

/// Decide whether to stop before the statement at `depth` on `line`, given the
/// active resume `mode` and whether `line` carries a breakpoint.
///
/// A breakpoint always wins and reports `breakpoint`, even mid-step, so a step
/// that crosses a breakpoint still honors it. Otherwise the step target governs:
/// a depth-relative target (`Over`/`Out`) stops once the run is back at or above
/// the issuing depth, and `Into`/`Pause` stop at the very next statement. Plain
/// `Continue` stops only at breakpoints.
pub fn decide(mode: Resume, depth: usize, line_has_breakpoint: bool) -> Option<StopReason> {
    if line_has_breakpoint {
        return Some(StopReason::Breakpoint);
    }
    match mode {
        Resume::Continue => None,
        Resume::Over { from_depth } if depth <= from_depth => Some(StopReason::Step),
        Resume::Over { .. } => None,
        Resume::Into => Some(StopReason::Step),
        Resume::Out { from_depth } if depth < from_depth => Some(StopReason::Step),
        Resume::Out { .. } => None,
        Resume::Pause => Some(StopReason::Pause),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_stops_only_on_a_breakpoint() {
        assert_eq!(decide(Resume::Continue, 1, false), None);
        assert_eq!(decide(Resume::Continue, 3, false), None);
        assert_eq!(
            decide(Resume::Continue, 3, true),
            Some(StopReason::Breakpoint)
        );
    }

    #[test]
    fn step_over_skips_deeper_statements_and_stops_at_same_depth() {
        let mode = Resume::Over { from_depth: 2 };
        // A statement inside a call the line made (depth 3) is stepped over.
        assert_eq!(decide(mode, 3, false), None);
        // Back at the issuing depth, we stop.
        assert_eq!(decide(mode, 2, false), Some(StopReason::Step));
        // Returning to a shallower caller also stops (the callee returned).
        assert_eq!(decide(mode, 1, false), Some(StopReason::Step));
    }

    #[test]
    fn step_over_still_honors_a_breakpoint_inside_the_call() {
        let mode = Resume::Over { from_depth: 1 };
        assert_eq!(decide(mode, 5, true), Some(StopReason::Breakpoint));
    }

    #[test]
    fn step_in_stops_at_the_very_next_statement_any_depth() {
        assert_eq!(decide(Resume::Into, 1, false), Some(StopReason::Step));
        assert_eq!(decide(Resume::Into, 9, false), Some(StopReason::Step));
    }

    #[test]
    fn step_out_stops_only_when_shallower_than_the_issuing_depth() {
        let mode = Resume::Out { from_depth: 3 };
        // Still inside the activation (same depth): keep running.
        assert_eq!(decide(mode, 3, false), None);
        // A nested call (deeper): keep running.
        assert_eq!(decide(mode, 4, false), None);
        // Returned to the caller (shallower): stop.
        assert_eq!(decide(mode, 2, false), Some(StopReason::Step));
    }

    #[test]
    fn pause_stops_at_the_next_statement_with_its_own_reason() {
        assert_eq!(decide(Resume::Pause, 1, false), Some(StopReason::Pause));
        // A breakpoint coinciding with a pause still reads as a breakpoint.
        assert_eq!(decide(Resume::Pause, 1, true), Some(StopReason::Breakpoint));
    }
}
