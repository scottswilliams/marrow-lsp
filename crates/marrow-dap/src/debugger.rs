//! The run-thread side of the debugger: the [`StepHook`] that parks the run at a
//! stop and serves the protocol thread's inspection queries without ever letting
//! the live [`Frame`] cross a thread boundary.
//!
//! ## The rendezvous
//! The interpreter is single-threaded and synchronous, so the run owns its store
//! and locals for as long as it executes. We run the run on a dedicated
//! *run-thread* with Marrow's debugger hook. [`StepHook::before_statement`] is
//! the rendezvous point: it records the current span and depth, asks the shared
//! [`step`](crate::step) policy whether this statement is a stop, and if so:
//!
//! 1. sends a [`RunEvent::Stopped`] to the *protocol thread* over a channel, then
//! 2. blocks in a serve-loop on its control channel.
//!
//! While parked *inside* `before_statement`, the run-thread still holds the
//! `Frame` borrow, so it — and only it — can read locals and the live store. The
//! protocol thread never touches the frame; it sends [`Control::Query`] messages
//! and reads back owned [`QueryResult`] values the run-thread computed. A
//! [`Control::Resume`] sets the next step mode and returns `Ok(())` so the run
//! proceeds; a [`Control::Terminate`] wakes a parked run, while an active run also
//! observes the shared termination flag before each statement. Either path returns
//! `Err`, which the runtime unwinds — rolling back any open transaction. The frame
//! borrow is thus confined to one thread by construction; the channels carry only
//! `Send` owned data.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use marrow_check::{CheckedDebugExpression, tooling};
use marrow_run::{DebugValue, Frame, RuntimeError, StepHook, Value};
use marrow_store::key::SavedKey;
use marrow_syntax::Diagnose;

use crate::step::{self, Resume, StopReason};
use crate::variables::{self, ChildPage, VariablesFilter};

/// The `run.terminated` fault code the hook returns to unwind the run on a client
/// `terminate`/`disconnect`. It surfaces like any runtime fault, so an open
/// `transaction` rolls back as the run stack unwinds.
pub const RUN_TERMINATED: &str = "run.terminated";

/// A snapshot of the locals visible at a stop. Captured on the run-thread from
/// Marrow runtime debug facts, so it is `Send` and the frame borrow stays put.
#[derive(Debug, Clone)]
pub struct LocalsSnapshot {
    pub visible_local_count: usize,
    pub locals_truncated: bool,
    pub entries: Vec<LocalVar>,
}

/// One local at a stop.
#[derive(Debug, Clone)]
pub struct LocalVar {
    pub name: String,
    pub value: String,
    pub children_truncated: Option<bool>,
    /// The owned debug value to expand, present only for a compound shape.
    pub expand: Option<DebugValue>,
}

/// One saved-data variable row captured from the stopped frame.
#[derive(Debug, Clone)]
pub struct DataVar {
    pub name: String,
    pub value: String,
    pub children_truncated: Option<bool>,
    pub path: Vec<tooling::DataPathSegment>,
    pub expandable: bool,
}

#[derive(Debug, Clone)]
pub struct DataVariablesSnapshot {
    pub entries: Vec<DataVar>,
    pub children_truncated: bool,
}

/// Where the run stopped, sent to the protocol thread so it can raise a DAP
/// `stopped` event and answer a `stackTrace` from cached data.
#[derive(Debug, Clone)]
pub struct StopInfo {
    pub reason: StopReason,
    pub span: marrow_syntax::SourceSpan,
    pub file: Option<PathBuf>,
    pub line: u32,
    pub column: u32,
    pub depth: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ArmedBreakpoints {
    breakpoints_by_file: HashMap<PathBuf, Vec<ArmedBreakpoint>>,
}

impl ArmedBreakpoints {
    pub fn insert(&mut self, file: PathBuf, breakpoint: ArmedBreakpoint) {
        let breakpoints = self.breakpoints_by_file.entry(file).or_default();
        if !breakpoints.contains(&breakpoint) {
            breakpoints.push(breakpoint);
        }
    }

    fn evaluate(&mut self, frame: &Frame<'_, '_>, line: u32) -> BreakpointAction {
        frame
            .file()
            .and_then(|file| self.breakpoints_by_file.get_mut(file))
            .map(|breakpoints| {
                breakpoints
                    .iter_mut()
                    .filter_map(|breakpoint| breakpoint.evaluate(frame, line))
                    .fold(BreakpointAction::default(), BreakpointAction::merge)
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedBreakpoint {
    line: u32,
    condition: Option<CheckedDebugExpression>,
    hit_target: Option<u64>,
    log_message: Option<String>,
    hits: u64,
}

impl ArmedBreakpoint {
    pub fn new(
        line: u32,
        condition: Option<CheckedDebugExpression>,
        hit_target: Option<u64>,
        log_message: Option<String>,
    ) -> Self {
        Self {
            line,
            condition,
            hit_target,
            log_message,
            hits: 0,
        }
    }
}

impl ArmedBreakpoint {
    fn evaluate(&mut self, frame: &Frame<'_, '_>, line: u32) -> Option<BreakpointAction> {
        if self.line != line {
            return None;
        }
        if let Some(condition) = &self.condition
            && !frame
                .evaluate_debug_expression(condition)
                .is_ok_and(|value| value == DebugValue::from_value(Value::Bool(true)))
        {
            return None;
        }
        self.hits = self.hits.saturating_add(1);
        if self.hit_target.is_some_and(|target| self.hits != target) {
            return None;
        }
        Some(match &self.log_message {
            Some(message) => BreakpointAction {
                outputs: vec![message.clone()],
                stop: false,
            },
            None => BreakpointAction {
                outputs: Vec::new(),
                stop: true,
            },
        })
    }
}

#[derive(Debug, Clone, Default)]
struct BreakpointAction {
    outputs: Vec<String>,
    stop: bool,
}

impl BreakpointAction {
    fn merge(mut self, next: Self) -> Self {
        self.outputs.extend(next.outputs);
        Self {
            outputs: self.outputs,
            stop: self.stop || next.stop,
        }
    }
}

/// A question the protocol thread asks the parked run-thread. Each is answered
/// from the live frame and store and returns owned, `Send` data.
#[derive(Debug)]
pub enum Query {
    /// The locals visible at the current stop.
    Locals {
        page: ChildPage,
        filter: VariablesFilter,
    },
    /// Evaluate a checked debug expression at the current stop.
    Evaluate {
        expression: Box<CheckedDebugExpression>,
    },
    DataChildren {
        segments: Vec<tooling::DataPathSegment>,
        page: ChildPage,
    },
}

/// The answer to a [`Query`], owned and `Send`.
#[derive(Debug)]
pub enum QueryResult {
    Locals(LocalsSnapshot),
    Evaluate(Result<DebugValue, RuntimeErrorDetail>),
    DataChildren(Result<DataVariablesSnapshot, RuntimeErrorDetail>),
}

/// The stable runtime-error detail carried back to the protocol thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeErrorDetail {
    pub code: String,
    pub message: String,
}

impl From<RuntimeError> for RuntimeErrorDetail {
    fn from(error: RuntimeError) -> Self {
        Self {
            code: error.code().to_string(),
            message: error.message().to_string(),
        }
    }
}

/// A control message from the protocol thread to the parked run-thread.
#[derive(Debug)]
pub enum Control {
    /// Answer a query and keep serving (the run stays parked).
    Query(Query),
    /// Replace the currently armed source-line breakpoints.
    SetBreakpoints(ArmedBreakpoints),
    /// Resume the run with the given step mode.
    Resume(Resume),
    /// Abort the run (terminate/disconnect): the hook returns `Err`.
    Terminate,
}

/// An event from the run-thread to the protocol thread.
#[derive(Debug)]
pub enum RunEvent {
    /// The run parked at a stop; the protocol thread should emit `stopped`.
    Stopped(StopInfo),
    /// A logpoint emitted console text and let the run continue.
    Output(String),
    /// The answer to the most recent [`Query`].
    Answer(QueryResult),
}

/// The hook installed on the run-thread. It owns the channel ends to the protocol
/// thread and the pending step mode.
pub struct Debugger {
    /// The step mode governing the *next* stop, updated each time the run resumes.
    mode: Resume,
    /// Set once at launch to stop before the first statement (`stopOnEntry`).
    stop_on_entry: bool,
    breakpoints: ArmedBreakpoints,
    /// True after the entry stop has been taken, so it fires at most once.
    entered: bool,
    events: Sender<RunEvent>,
    control: Receiver<Control>,
    terminate: Arc<AtomicBool>,
}

impl Debugger {
    /// Build the hook for a launch: whether to stop on entry, and the channel ends.
    pub fn new(
        stop_on_entry: bool,
        breakpoints: ArmedBreakpoints,
        events: Sender<RunEvent>,
        control: Receiver<Control>,
        terminate: Arc<AtomicBool>,
    ) -> Self {
        Debugger {
            // Until the client steps, run freely unless stop-on-entry fires.
            mode: Resume::Continue,
            stop_on_entry,
            breakpoints,
            entered: false,
            events,
            control,
            terminate,
        }
    }

    /// Whether this is the entry stop to take once for `stopOnEntry`.
    fn is_entry_stop(&mut self) -> bool {
        if self.stop_on_entry && !self.entered {
            self.entered = true;
            return true;
        }
        false
    }

    /// Capture the locals at the current frame as owned, `Send` data.
    fn snapshot_locals(
        frame: &Frame<'_, '_>,
        page: ChildPage,
        filter: VariablesFilter,
    ) -> LocalsSnapshot {
        let snapshot =
            frame.debug_snapshot(page.to_debug_value_page(), filter.to_debug_value_filter());
        let entries = variables::local_children(snapshot.locals)
            .into_iter()
            .map(|child| LocalVar {
                name: child.name,
                value: child.value,
                children_truncated: child.children_truncated,
                expand: child.expand,
            })
            .collect();
        LocalsSnapshot {
            visible_local_count: snapshot.visible_local_count,
            locals_truncated: snapshot.locals_truncated,
            entries,
        }
    }

    fn snapshot_data_children(
        frame: &Frame<'_, '_>,
        segments: Vec<tooling::DataPathSegment>,
        page: ChildPage,
    ) -> Result<DataVariablesSnapshot, RuntimeErrorDetail> {
        let (start, limit) = data_page_bounds(page);
        let page = data_children_at_offset(frame, &segments, start, limit)?;
        let children_truncated = page.data.truncated;
        let mut entries = Vec::with_capacity(page.data.children.len());
        for child in page.data.children {
            let mut path = segments.clone();
            path.push(data_child_segment(child));
            let preview = frame
                .debug_data_preview(&path, marrow_check::tooling::DEFAULT_VALUE_PREVIEW_LIMIT)
                .map_err(RuntimeErrorDetail::from)?;
            let presence = preview
                .as_ref()
                .map(|preview| preview.data.presence)
                .unwrap_or(tooling::DataPresence::Absent);
            let value = preview
                .and_then(|preview| preview.data.preview.map(|preview| preview.text))
                .unwrap_or_default();
            entries.push(DataVar {
                name: tooling::render_data_path_segments(&path),
                value,
                children_truncated: None,
                expandable: matches!(presence, tooling::DataPresence::ChildrenOnly),
                path,
            });
        }
        Ok(DataVariablesSnapshot {
            entries,
            children_truncated,
        })
    }

    /// Answer one query from the live frame, returning owned data.
    fn answer(
        &self,
        frame: &Frame<'_, '_>,
        _span: marrow_syntax::SourceSpan,
        query: Query,
    ) -> QueryResult {
        match query {
            Query::Locals { page, filter } => {
                QueryResult::Locals(Self::snapshot_locals(frame, page, filter))
            }
            Query::Evaluate { expression } => QueryResult::Evaluate(
                frame
                    .evaluate_debug_expression(expression.as_ref())
                    .map_err(RuntimeErrorDetail::from),
            ),
            Query::DataChildren { segments, page } => {
                QueryResult::DataChildren(Self::snapshot_data_children(frame, segments, page))
            }
        }
    }

    /// Park at a stop: announce it, then serve queries until the protocol thread
    /// resumes or terminates. Returns `Ok(())` to continue the run or `Err` to
    /// unwind it (terminate). The frame is borrowed only here, on this thread.
    fn park(&mut self, info: StopInfo, frame: Frame<'_, '_>) -> Result<(), RuntimeError> {
        let span = info.span;
        if self.events.send(RunEvent::Stopped(info)).is_err() {
            // The protocol thread is gone; unwind the run rather than hang.
            return Err(terminated());
        }
        loop {
            match self.control.recv() {
                Ok(Control::Query(query)) => {
                    let answer = self.answer(&frame, span, query);
                    if self.events.send(RunEvent::Answer(answer)).is_err() {
                        return Err(terminated());
                    }
                }
                Ok(Control::SetBreakpoints(breakpoints)) => {
                    self.breakpoints = breakpoints;
                }
                Ok(Control::Resume(mode)) => {
                    self.mode = mode;
                    return Ok(());
                }
                // A terminate, a disconnect, or a dropped channel all unwind.
                Ok(Control::Terminate) | Err(_) => return Err(terminated()),
            }
        }
    }
}

fn data_page_bounds(page: ChildPage) -> (usize, usize) {
    match page {
        ChildPage::All { start } => (start, marrow_check::tooling::MAX_PREVIEW_ITEMS),
        ChildPage::Range { start, count } => (
            start,
            count.clamp(1, marrow_check::tooling::MAX_PREVIEW_ITEMS),
        ),
    }
}

fn data_children_at_offset(
    frame: &Frame<'_, '_>,
    segments: &[tooling::DataPathSegment],
    start: usize,
    limit: usize,
) -> Result<tooling::StampedData<tooling::DataChildrenPage>, RuntimeErrorDetail> {
    data_children_at_offset_with_limit(
        start,
        limit,
        marrow_check::tooling::MAX_PREVIEW_ITEMS,
        |cursor, page_limit| {
            frame
                .debug_data_children(segments, page_limit, cursor)
                .map_err(RuntimeErrorDetail::from)
        },
    )
}

fn data_children_at_offset_with_limit(
    start: usize,
    limit: usize,
    page_limit: usize,
    mut fetch: impl FnMut(
        Option<&SavedKey>,
        usize,
    )
        -> Result<tooling::StampedData<tooling::DataChildrenPage>, RuntimeErrorDetail>,
) -> Result<tooling::StampedData<tooling::DataChildrenPage>, RuntimeErrorDetail> {
    let page_limit = page_limit.max(1);
    let mut skipped: usize = 0;
    let mut cursor = None;
    let mut children = Vec::new();
    loop {
        let page = fetch(cursor.as_ref(), page_limit)?;
        let stamp = page.stamp;
        let data = page.data;
        let page_len = data.children.len();
        let mut more_in_page = false;
        for (index, child) in data.children.into_iter().enumerate() {
            if skipped.saturating_add(index) < start {
                continue;
            }
            if children.len() == limit {
                more_in_page = true;
                break;
            }
            children.push(child);
        }

        skipped = skipped.saturating_add(page_len);
        if children.len() == limit {
            return Ok(data_children_page(
                stamp,
                children,
                more_in_page || data.truncated,
            ));
        }
        if !data.truncated || page_len == 0 {
            return Ok(data_children_page(stamp, children, false));
        }
        let Some(next_cursor) = data.cursor else {
            return Ok(data_children_page(stamp, children, false));
        };
        cursor = Some(next_cursor);
    }
}

fn data_children_page(
    stamp: tooling::DataSnapshotStamp,
    children: Vec<tooling::DataChild>,
    truncated: bool,
) -> tooling::StampedData<tooling::DataChildrenPage> {
    tooling::StampedData {
        data: tooling::DataChildrenPage {
            children,
            truncated,
            cursor: None,
        },
        stamp,
    }
}

fn data_child_segment(child: tooling::DataChild) -> tooling::DataPathSegment {
    match child {
        tooling::DataChild::Root(root) => tooling::DataPathSegment::Root(root),
        tooling::DataChild::Key(key) => tooling::DataPathSegment::Key(key),
        tooling::DataChild::Field(field) => tooling::DataPathSegment::Field(field),
        tooling::DataChild::Layer(layer) => tooling::DataPathSegment::Layer(layer),
    }
}

impl StepHook for Debugger {
    fn before_statement(
        &mut self,
        span: marrow_syntax::SourceSpan,
        frame: Frame<'_, '_>,
    ) -> Result<(), RuntimeError> {
        if self.terminate.load(Ordering::Relaxed) {
            return Err(terminated());
        }
        let depth = frame.depth();
        let breakpoint = self.breakpoints.evaluate(&frame, span.line);
        for output in breakpoint.outputs {
            if self.events.send(RunEvent::Output(output)).is_err() {
                return Err(terminated());
            }
        }
        let reason = if self.is_entry_stop() {
            Some(StopReason::Entry)
        } else if let Some(reason) = step::decide(self.mode, depth) {
            Some(reason)
        } else if breakpoint.stop {
            Some(StopReason::Breakpoint)
        } else {
            None
        };

        let Some(reason) = reason else {
            return Ok(());
        };
        let info = StopInfo {
            reason,
            span,
            file: frame.file().map(PathBuf::from),
            line: span.line,
            column: span.column,
            depth,
        };
        self.park(info, frame)
    }
}

/// The terminate fault returned to unwind the run on a client terminate or a lost
/// protocol thread. It carries the [`RUN_TERMINATED`] code so the surfaced fault
/// is recognizable and any open transaction rolls back as the stack unwinds.
fn terminated() -> RuntimeError {
    RuntimeError::fatal(
        RUN_TERMINATED,
        "run terminated by the debugger",
        marrow_syntax::SourceSpan::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminated_fault_carries_the_terminated_code() {
        assert_eq!(terminated().code(), RUN_TERMINATED);
    }

    #[test]
    fn data_offset_walks_cursor_pages() {
        let mut pages = vec![
            data_page([1, 2], true, Some(2)),
            data_page([3, 4], true, Some(4)),
            data_page([5], false, None),
        ]
        .into_iter();
        let mut calls = Vec::new();

        let page = data_children_at_offset_with_limit(3, 2, 2, |cursor, limit| {
            calls.push((cursor.cloned(), limit));
            Ok(pages.next().expect("test page"))
        })
        .expect("page");

        assert_eq!(data_child_keys(&page.data.children), vec![4, 5]);
        assert!(!page.data.truncated);
        assert_eq!(
            calls,
            vec![
                (None, 2),
                (Some(SavedKey::Int(2)), 2),
                (Some(SavedKey::Int(4)), 2),
            ]
        );
    }

    #[test]
    fn data_offset_preserves_truncation_when_page_has_more_children() {
        let mut pages = vec![
            data_page([1, 2], true, Some(2)),
            data_page([3, 4], true, Some(4)),
        ]
        .into_iter();

        let page = data_children_at_offset_with_limit(1, 2, 2, |_, _| {
            Ok(pages.next().expect("test page"))
        })
        .expect("page");

        assert_eq!(data_child_keys(&page.data.children), vec![2, 3]);
        assert!(page.data.truncated);
    }

    fn data_page<const N: usize>(
        keys: [i64; N],
        truncated: bool,
        cursor: Option<i64>,
    ) -> tooling::StampedData<tooling::DataChildrenPage> {
        tooling::StampedData {
            data: tooling::DataChildrenPage {
                children: keys
                    .into_iter()
                    .map(|key| tooling::DataChild::Key(SavedKey::Int(key)))
                    .collect(),
                truncated,
                cursor: cursor.map(SavedKey::Int),
            },
            stamp: data_stamp(),
        }
    }

    fn data_child_keys(children: &[tooling::DataChild]) -> Vec<i64> {
        children
            .iter()
            .map(|child| match child {
                tooling::DataChild::Key(SavedKey::Int(key)) => *key,
                other => panic!("unexpected data child: {other:?}"),
            })
            .collect()
    }

    fn data_stamp() -> tooling::DataSnapshotStamp {
        tooling::DataSnapshotStamp {
            store_uid: None,
            store_catalog_digest: None,
            store_commit: None,
            open_transaction: None,
            checked_source_digest: "test".to_string(),
        }
    }
}
