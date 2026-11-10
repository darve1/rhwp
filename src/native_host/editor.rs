//! Opt-in native editor state. The document engine itself remains history-free.
//!
//! This surface edits body text and table-cell text. A selection stays inside
//! one container: the body paragraphs of a section, or the paragraphs of one
//! (possibly nested) table cell. Offsets are Unicode scalar indices on the
//! core's logical caret axis: an inline (treat-as-char) object, footnote or
//! endnote takes one offset and shows as U+FFFC in paragraph text. Text edits
//! never remove objects, and tabs are rejected explicitly. Read-only selection
//! may span objects.
//! Paragraph split/join and grapheme navigation live in `commands`. OS IME events
//! and screen coordinates belong to the host, not to this session.

use std::collections::VecDeque;

use std::borrow::Cow;

use crate::document_core::native_interaction::{
    native_paragraph_text, native_to_text_offset, NativeTextPosition, NativeTextSelection,
};
use crate::model::control::Control;
use crate::model::paragraph::Paragraph;
use crate::DocumentCore;

use super::{NativeDocumentSession, NativeHostError};

#[path = "ime_edit.rs"]
pub mod ime_edit;

#[path = "actions.rs"]
pub mod actions;

const DEFAULT_HISTORY_CAPACITY: usize = 16;

/// The host must choose what losing input focus does to an active composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeCompositionFocusLoss {
    Commit,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeEditOutcome {
    pub revision: u64,
    pub selection: NativeTextSelection,
    /// This call changed document contents, including an IME preview or rollback.
    pub document_changed: bool,
    /// This call changed undo/redo history; committing a preview need not repaint.
    pub history_changed: bool,
    /// Pass this identifier back with IME events to reject late events from a
    /// previous composition even when the document revision has not changed.
    pub composition_id: Option<u64>,
}

struct HistoryEntry {
    before: u32,
    after: u32,
    selection_before: NativeTextSelection,
    selection_after: NativeTextSelection,
}

struct Composition {
    id: u64,
    before: u32,
    selection_before: NativeTextSelection,
    /// The replaced span can differ from the user's undo/cancel selection when
    /// an atomic IME event supplies an explicit surrounding-text range.
    replacement_range: NativeTextSelection,
    original_text: String,
    preview_text: Option<String>,
    changed: bool,
}

/// A single owner of document edits, selection, composition and bounded history.
///
/// A snapshot pair is deliberately retained for each committed action in this
/// initial API. The capacity bounds snapshot count, not document byte size.
/// Existing snapshots owned by the caller are preserved. Two free slots cover
/// either a new action's before/after pair or composition plus rollback state.
/// The core event log keeps its existing semantics; this is not a total-memory
/// budget or a promise to rewind previously emitted events on undo.
pub struct NativeEditor {
    session: NativeDocumentSession,
    selection: NativeTextSelection,
    undo: VecDeque<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    history_capacity: usize,
    composition: Option<Composition>,
    next_composition_id: u64,
}

impl NativeEditor {
    pub fn new(
        session: NativeDocumentSession,
        selection: NativeTextSelection,
    ) -> Result<Self, NativeHostError> {
        Self::try_new(session, selection).map_err(|failed| {
            let (_, error) = *failed;
            error
        })
    }

    /// Enter editing without losing the viewer session when its selected target
    /// is unsupported. The returned session is unchanged on every error path.
    /// Hosts that retain an open document should use this instead of `new`.
    pub fn try_new(
        session: NativeDocumentSession,
        selection: NativeTextSelection,
    ) -> Result<Self, Box<(NativeDocumentSession, NativeHostError)>> {
        let available = Self::available_history_capacity(&session);
        Self::try_with_history_capacity(session, selection, DEFAULT_HISTORY_CAPACITY.min(available))
    }

    pub fn with_history_capacity(
        session: NativeDocumentSession,
        selection: NativeTextSelection,
        history_capacity: usize,
    ) -> Result<Self, NativeHostError> {
        Self::try_with_history_capacity(session, selection, history_capacity).map_err(|failed| {
            let (_, error) = *failed;
            error
        })
    }

    fn try_with_history_capacity(
        session: NativeDocumentSession,
        selection: NativeTextSelection,
        history_capacity: usize,
    ) -> Result<Self, Box<(NativeDocumentSession, NativeHostError)>> {
        if session.core.batch_mode {
            return Err(Box::new((
                session,
                NativeHostError::Unsupported(
                    "finish the document batch before opening a native editor".into(),
                ),
            )));
        }
        if let Err(error) = Self::validate_selection(&session.core, &selection) {
            return Err(Box::new((session, error)));
        }
        let available = Self::available_history_capacity(&session);
        if history_capacity == 0 || history_capacity > available {
            return Err(Box::new((
                session,
                NativeHostError::LimitExceeded(format!(
                    "native editor history capacity must be in 1..={available}"
                )),
            )));
        }
        Ok(Self {
            session,
            selection,
            undo: VecDeque::new(),
            redo: Vec::new(),
            history_capacity,
            composition: None,
            next_composition_id: 1,
        })
    }

    /// View state only: no revision change, no history entry.
    pub fn set_editor_marks(&mut self, marks: super::NativeEditorMarks) {
        self.session.set_editor_marks(marks);
    }

    pub fn session(&self) -> &NativeDocumentSession {
        &self.session
    }

    /// Keep history and selection while replacing fonts; finish IME first.
    pub fn set_font_sources(
        &mut self,
        expected_revision: u64,
        paths: &[std::path::PathBuf],
        environment: Option<crate::renderer::font_environment::FontEnvironment>,
    ) -> Result<bool, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        self.require_no_composition()?;
        self.session.set_font_sources(paths, environment)
    }

    /// Save without committing an unfinished IME preview. HWP stores current
    /// body focus; with the focus in a table cell, whose caret the saved-caret
    /// contract does not cover, it keeps the stored caret instead. HWPX
    /// retains its existing source-settings/default policy.
    pub fn prepare_save(
        &mut self,
        format: super::save::NativeSaveFormat,
    ) -> Result<super::save::PreparedSave, NativeHostError> {
        self.require_no_composition()?;
        match format {
            super::save::NativeSaveFormat::Hwp if self.selection.focus.cell_path.is_empty() => self
                .session
                .prepare_save_with_caret(format, &self.selection.focus),
            _ => self.session.prepare_save(format),
        }
    }

    pub fn acknowledge_save(
        &mut self,
        token: super::save::NativeSaveToken,
    ) -> Result<super::save::NativeSaveStatus, NativeHostError> {
        self.session.acknowledge_save(token)
    }

    pub fn cancel_save(
        &mut self,
        token: super::save::NativeSaveToken,
    ) -> Result<super::save::NativeSaveStatus, NativeHostError> {
        self.session.cancel_save(token)
    }

    pub fn selection(&self) -> &NativeTextSelection {
        &self.selection
    }

    pub fn can_undo(&self) -> bool {
        self.composition.is_none() && !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        self.composition.is_none() && !self.redo.is_empty()
    }

    pub fn history_capacity(&self) -> usize {
        self.history_capacity
    }

    pub fn composition_id(&self) -> Option<u64> {
        self.composition.as_ref().map(|composition| composition.id)
    }

    /// Return the session after releasing this editor's undo resources. An active
    /// composition returns the editor intact so the host can commit or cancel it.
    pub fn into_session(mut self) -> Result<NativeDocumentSession, Box<Self>> {
        if self.composition.is_some() {
            return Err(Box::new(self));
        }
        self.discard_history();
        Ok(self.session)
    }

    pub fn set_selection(
        &mut self,
        expected_revision: u64,
        selection: NativeTextSelection,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        self.require_no_composition()?;
        Self::validate_selection(&self.session.core, &selection)?;
        self.selection = selection;
        Ok(self.outcome(false, false))
    }

    /// Insert at a collapsed selection, delete with an empty string, or replace a
    /// range. The selected body paragraphs may span one section. Newline and tab
    /// input are not silently interpreted as paragraph/control insertion.
    pub fn replace_selection(
        &mut self,
        expected_revision: u64,
        text: &str,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        let selection = self.selection.clone();
        self.replace_range(expected_revision, &selection, text)
    }

    /// Shared scalar range replacement for editor commands and host adapters.
    /// The undo selection is the caller's actual selection, not this edit range.
    pub(super) fn replace_range(
        &mut self,
        expected_revision: u64,
        selection: &NativeTextSelection,
        text: &str,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.check_edit_ready(expected_revision)?;
        Self::validate_text(text)?;
        Self::validate_selection(&self.session.core, selection)?;
        let selection_after = Self::replacement_end(selection, text)?;
        if self.selected_text(selection) == text {
            self.selection = selection_after;
            return Ok(self.outcome(false, false));
        }
        self.edit_transaction(expected_revision, |core| {
            Self::apply_replacement(core, selection, text)?;
            Ok(selection_after)
        })
    }

    pub(super) fn check_edit_ready(&self, expected_revision: u64) -> Result<(), NativeHostError> {
        self.session.check_revision(expected_revision)?;
        self.require_no_composition()?;
        Self::validate_selection(&self.session.core, &self.selection)
    }

    /// Commit one known-changing action. The closure can mutate only the core;
    /// editor selection/history are published after every fallible step succeeds.
    /// This does not rewind the core's already-emitted event log on rollback.
    pub(super) fn edit_transaction(
        &mut self,
        expected_revision: u64,
        edit: impl FnOnce(&mut DocumentCore) -> Result<NativeTextSelection, NativeHostError>,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.check_edit_ready(expected_revision)?;
        Self::check_body_text_span(&self.session.core, &self.selection)?;
        self.check_mutation_capacity()?;
        self.check_snapshot_slots(2)?;
        let selection_before = self.selection.clone();
        let before = self.session.core.save_snapshot_native();
        let result = edit(&mut self.session.core).and_then(|selection| {
            Self::validate_selection(&self.session.core, &selection)?;
            Ok(selection)
        });
        let selection_after = match result {
            Ok(selection) => selection,
            Err(error) => return Err(self.rollback_and_discard(before, error)),
        };
        if let Err(error) = self.session.did_mutate() {
            return Err(self.rollback_and_discard(before, error));
        }
        let after = self.session.core.save_snapshot_native();
        self.selection = selection_after.clone();
        self.record(HistoryEntry {
            before,
            after,
            selection_before,
            selection_after,
        });
        Ok(self.outcome(true, true))
    }

    pub fn begin_composition(
        &mut self,
        expected_revision: u64,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        self.require_no_composition()?;
        Self::validate_selection(&self.session.core, &self.selection)?;
        self.check_snapshot_slots(2)?;
        let next_id = self.next_composition_id.checked_add(1).ok_or_else(|| {
            NativeHostError::LimitExceeded("composition identifiers exhausted".into())
        })?;
        let original_text = self.selected_text(&self.selection);
        let before = self.session.core.save_snapshot_native();
        self.composition = Some(Composition {
            id: self.next_composition_id,
            before,
            selection_before: self.selection.clone(),
            replacement_range: self.selection.clone(),
            original_text,
            preview_text: None,
            changed: false,
        });
        self.next_composition_id = next_id;
        Ok(self.outcome(false, false))
    }

    /// Preview from the original selection each time. This preserves formatting
    /// on composition cancellation and never records intermediate Korean jamo.
    pub fn update_composition(
        &mut self,
        expected_revision: u64,
        composition_id: u64,
        text: &str,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        let composition = self.require_composition(composition_id)?;
        Self::validate_text(text)?;
        if composition.preview_text.as_deref() == Some(text) {
            return Ok(self.outcome(false, false));
        }
        let selection_before = composition.replacement_range.clone();
        let selection_after = Self::replacement_end(&selection_before, text)?;
        let before = composition.before;
        let changed = text != composition.original_text;
        let needs_mutation = changed || composition.changed;
        if needs_mutation {
            self.check_mutation_capacity()?;
            self.check_snapshot_slots(1)?;
            let rollback = self.session.core.save_snapshot_native();
            let result = self
                .session
                .core
                .restore_snapshot_native(before)
                .map_err(NativeHostError::from)
                .and_then(|_| {
                    if changed {
                        Self::apply_replacement(&mut self.session.core, &selection_before, text)
                    } else {
                        Ok(())
                    }
                })
                .and_then(|_| self.session.did_mutate().map(|_| ()));
            if let Err(error) = result {
                return Err(self.rollback_and_discard(rollback, error));
            }
            self.session.core.discard_snapshot_native(rollback);
        }
        self.selection = selection_after;
        let composition = self.composition.as_mut().expect("validated composition");
        composition.preview_text = Some(text.to_owned());
        composition.changed = changed;
        Ok(self.outcome(needs_mutation, false))
    }

    /// Record already-applied preedit exactly once. The host sends its final
    /// committed text through update_composition before calling this method.
    pub fn commit_composition(
        &mut self,
        expected_revision: u64,
        composition_id: u64,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        let changed = self.require_composition(composition_id)?.changed;
        if changed {
            self.check_snapshot_slots(1)?;
        }
        let composition = self.composition.take().expect("validated composition");
        if changed {
            let after = self.session.core.save_snapshot_native();
            self.record(HistoryEntry {
                before: composition.before,
                after,
                selection_before: composition.selection_before,
                selection_after: self.selection.clone(),
            });
        } else {
            self.session
                .core
                .discard_snapshot_native(composition.before);
        }
        Ok(self.outcome(false, changed))
    }

    pub fn cancel_composition(
        &mut self,
        expected_revision: u64,
        composition_id: u64,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        let composition = self.require_composition(composition_id)?;
        let changed = composition.changed;
        let before = composition.before;
        if changed {
            self.check_mutation_capacity()?;
            self.session.core.restore_snapshot_native(before)?;
            self.session.did_mutate()?;
        }
        let composition = self.composition.take().expect("validated composition");
        self.selection = composition.selection_before;
        self.session.core.discard_snapshot_native(before);
        Ok(self.outcome(changed, false))
    }

    pub fn focus_lost(
        &mut self,
        expected_revision: u64,
        policy: NativeCompositionFocusLoss,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        match (self.composition_id(), policy) {
            (Some(id), NativeCompositionFocusLoss::Commit) => {
                self.commit_composition(expected_revision, id)
            }
            (Some(id), NativeCompositionFocusLoss::Cancel) => {
                self.cancel_composition(expected_revision, id)
            }
            (None, _) => Ok(self.outcome(false, false)),
        }
    }

    pub fn undo(&mut self, expected_revision: u64) -> Result<NativeEditOutcome, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        self.require_no_composition()?;
        let Some(entry) = self.undo.back() else {
            return Ok(self.outcome(false, false));
        };
        self.check_mutation_capacity()?;
        self.session.core.restore_snapshot_native(entry.before)?;
        self.session.did_mutate()?;
        let entry = self.undo.pop_back().expect("checked history entry");
        self.selection = entry.selection_before.clone();
        self.redo.push(entry);
        Ok(self.outcome(true, true))
    }

    pub fn redo(&mut self, expected_revision: u64) -> Result<NativeEditOutcome, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        self.require_no_composition()?;
        let Some(entry) = self.redo.last() else {
            return Ok(self.outcome(false, false));
        };
        self.check_mutation_capacity()?;
        self.session.core.restore_snapshot_native(entry.after)?;
        self.session.did_mutate()?;
        let entry = self.redo.pop().expect("checked history entry");
        self.selection = entry.selection_after.clone();
        self.undo.push_back(entry);
        Ok(self.outcome(true, true))
    }

    pub fn clear_history(
        &mut self,
        expected_revision: u64,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.session.check_revision(expected_revision)?;
        self.require_no_composition()?;
        let changed = !self.undo.is_empty() || !self.redo.is_empty();
        self.discard_history();
        Ok(self.outcome(false, changed))
    }

    fn available_history_capacity(session: &NativeDocumentSession) -> usize {
        DocumentCore::MAX_SNAPSHOTS.saturating_sub(session.core.snapshot_store.len() + 2) / 2
    }

    fn check_snapshot_slots(&self, count: usize) -> Result<(), NativeHostError> {
        if self.session.core.snapshot_store.len() + count > DocumentCore::MAX_SNAPSHOTS
            || self
                .session
                .core
                .next_snapshot_id
                .checked_add(count as u32)
                .is_none()
        {
            return Err(NativeHostError::LimitExceeded(
                "native editor snapshot budget exhausted".into(),
            ));
        }
        Ok(())
    }

    fn check_mutation_capacity(&self) -> Result<(), NativeHostError> {
        self.session
            .revision()
            .checked_add(1)
            .ok_or_else(|| NativeHostError::LimitExceeded("document revision exhausted".into()))
            .map(|_| ())
    }

    fn require_no_composition(&self) -> Result<(), NativeHostError> {
        if self.composition.is_some() {
            Err(NativeHostError::InvalidArgument(
                "commit or cancel the active composition first".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn require_composition(&self, id: u64) -> Result<&Composition, NativeHostError> {
        self.composition
            .as_ref()
            .filter(|composition| composition.id == id)
            .ok_or_else(|| {
                NativeHostError::InvalidArgument("inactive or stale composition identifier".into())
            })
    }

    fn validate_text(text: &str) -> Result<(), NativeHostError> {
        if text.chars().any(char::is_control) {
            return Err(NativeHostError::Unsupported(
                "body text input does not insert control characters, tabs or paragraphs".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn validate_selection(
        core: &DocumentCore,
        selection: &NativeTextSelection,
    ) -> Result<(), NativeHostError> {
        for position in [&selection.anchor, &selection.focus] {
            // A cell path into a control that is not a table stays "unsupported".
            core.validate_native_text_position(position)
                .map_err(NativeHostError::from)?;
        }
        if selection.anchor.section != selection.focus.section {
            return Err(NativeHostError::Unsupported(
                "cross-section text replacement is not supported".into(),
            ));
        }
        if !Self::same_container(&selection.anchor, &selection.focus) {
            return Err(NativeHostError::Unsupported(
                "a selection cannot cross a table-cell boundary".into(),
            ));
        }
        let (start, end) = Self::ordered(selection);
        for paragraph in Self::leaf(start)..=Self::leaf(end) {
            core.check_native_interaction_support(&Self::at_leaf(start, paragraph, 0))
                .map_err(|error| NativeHostError::Unsupported(error.to_string()))?;
        }
        Ok(())
    }

    pub(super) fn ordered(
        selection: &NativeTextSelection,
    ) -> (&NativeTextPosition, &NativeTextPosition) {
        if (
            Self::leaf(&selection.anchor),
            selection.anchor.scalar_offset,
        ) <= (Self::leaf(&selection.focus), selection.focus.scalar_offset)
        {
            (&selection.anchor, &selection.focus)
        } else {
            (&selection.focus, &selection.anchor)
        }
    }

    /// The scalar edit surface removes text, never objects: a range may hold
    /// paragraphs with controls (fields, bookmarks, floating pictures and
    /// tables...), but no control may be anchored strictly inside it, where
    /// removing the text would also remove the control. A control at text
    /// position `p` sits before character `p`, so one at either end of the
    /// range stays. An inline object fills its own offset, so a range that
    /// covers it is refused. Inserting at a caret never removes a control.
    /// Keep this separate from selection validation: read-only navigation and
    /// identical-text no-ops do not need to modify an object's paragraph.
    pub(super) fn check_body_text_span(
        core: &DocumentCore,
        selection: &NativeTextSelection,
    ) -> Result<(), NativeHostError> {
        let (start, end) = Self::ordered(selection);
        let (first, last) = (Self::leaf(start), Self::leaf(end));
        let paragraphs = Self::container(core, start)?;
        for index in first..=last {
            let paragraph = &paragraphs[index as usize];
            let after = if index == first {
                start.scalar_offset as usize
            } else {
                0
            };
            let before = end.scalar_offset as usize;
            // Removed: [after, before) of a single paragraph; the tail of the
            // first and the head of the last paragraph of a longer range (they
            // merge); every middle paragraph whole. Offsets are logical: an
            // inline object fills offset `q`, any other control sits before it.
            let removes = |position: usize, inline: bool| match (index == first, index == last) {
                (true, true) => {
                    (position > after || inline && position == after) && position < before
                }
                (true, false) => position > after || inline && position == after,
                (false, true) => position < before,
                (false, false) => true,
            };
            let anchored = paragraph
                .logical_control_positions()
                .into_iter()
                .zip(&paragraph.controls)
                .any(|(position, control)| {
                    !matches!(control, Control::SectionDef(_) | Control::ColumnDef(_))
                        && removes(position, control.is_logical_inline())
                });
            if anchored {
                return Err(NativeHostError::Unsupported(
                    "the range holds an object; text edits do not remove objects".into(),
                ));
            }
        }
        Ok(())
    }

    /// Imported paragraphs (HTML paste) must be plain text: no controls other
    /// than the section/column definitions a blank document carries.
    pub(super) fn check_plain_paragraphs(
        core: &DocumentCore,
        selection: &NativeTextSelection,
    ) -> Result<(), NativeHostError> {
        let (start, end) = Self::ordered(selection);
        for paragraph in
            &Self::container(core, start)?[Self::leaf(start) as usize..=Self::leaf(end) as usize]
        {
            if paragraph.controls.iter().any(|control| {
                // Blank documents carry these section/column definitions; core
                // text editing preserves them. Other controls (including fields,
                // bookmarks, headers and unknown controls) are not plain text.
                !matches!(control, Control::SectionDef(_) | Control::ColumnDef(_))
            }) {
                return Err(NativeHostError::Unsupported(
                    "body text edits and clipboard export cannot include semantic-control paragraphs"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    fn replacement_end(
        selection: &NativeTextSelection,
        text: &str,
    ) -> Result<NativeTextSelection, NativeHostError> {
        let (start, _) = Self::ordered(selection);
        let length = u32::try_from(text.chars().count()).map_err(|_| {
            NativeHostError::LimitExceeded("input text exceeds the scalar address range".into())
        })?;
        let mut end = start.clone();
        end.scalar_offset = end.scalar_offset.checked_add(length).ok_or_else(|| {
            NativeHostError::LimitExceeded("input text exceeds the scalar address range".into())
        })?;
        Ok(NativeTextSelection {
            anchor: end.clone(),
            focus: end,
        })
    }

    pub(super) fn selected_text(&self, selection: &NativeTextSelection) -> String {
        let (start, end) = Self::ordered(selection);
        let Ok(paragraphs) = Self::container(&self.session.core, start) else {
            return String::new();
        };
        let (first, last) = (Self::leaf(start), Self::leaf(end));
        (first..=last)
            .map(|paragraph| {
                let text = native_paragraph_text(&paragraphs[paragraph as usize]);
                let from = if paragraph == first {
                    start.scalar_offset as usize
                } else {
                    0
                };
                let to = if paragraph == last {
                    end.scalar_offset as usize
                } else {
                    text.chars().count()
                };
                text.chars().skip(from).take(to - from).collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn apply_replacement(
        core: &mut DocumentCore,
        selection: &NativeTextSelection,
        text: &str,
    ) -> Result<(), NativeHostError> {
        // Also covers IME previews and command edit ranges distinct from the
        // editor's current selection. This check precedes all core mutations.
        Self::check_form_mode(core, Some(selection))?;
        Self::check_body_text_span(core, selection)?;
        let (start, end) = Self::ordered(selection);
        if !start.cell_path.is_empty() {
            if start != end {
                Self::delete_in_container(core, start, end)?;
            }
            if !text.is_empty() {
                Self::insert_in_container(core, start, text)?;
            }
            Self::recalculate_formulas(core, start)?;
            return Ok(());
        }
        if start.paragraph == end.paragraph
            && end.scalar_offset - start.scalar_offset <= 8
            && text.chars().count() <= 8
            && !Self::paragraph_at(core, start)?
                .controls
                .iter()
                .any(Control::is_logical_inline)
        {
            core.replace_body_text_local_native(
                start.section as usize,
                start.paragraph as usize,
                start.scalar_offset as usize,
                (end.scalar_offset - start.scalar_offset) as usize,
                text,
            )?;
            // Expose a settled document to native geometry/save consumers. This
            // session does not yet own Studio's deferred-pagination scheduler.
            core.paginate_if_needed();
            return Ok(());
        }
        if start != end {
            Self::delete_in_container(core, start, end)?;
        }
        if !text.is_empty() {
            Self::insert_in_container(core, start, text)?;
        }
        Ok(())
    }

    fn rollback_and_discard(
        &mut self,
        snapshot: u32,
        original_error: NativeHostError,
    ) -> NativeHostError {
        let restored = self.session.core.restore_snapshot_native(snapshot);
        self.session.core.discard_snapshot_native(snapshot);
        match restored {
            Ok(_) => original_error,
            Err(error) => NativeHostError::Core(error),
        }
    }

    fn record(&mut self, entry: HistoryEntry) {
        while let Some(entry) = self.redo.pop() {
            self.discard_entry(entry);
        }
        self.undo.push_back(entry);
        while self.undo.len() > self.history_capacity {
            let entry = self.undo.pop_front().expect("history over capacity");
            self.discard_entry(entry);
        }
    }

    fn discard_entry(&mut self, entry: HistoryEntry) {
        self.session.core.discard_snapshot_native(entry.before);
        self.session.core.discard_snapshot_native(entry.after);
    }

    fn discard_history(&mut self) {
        while let Some(entry) = self.undo.pop_front() {
            self.discard_entry(entry);
        }
        while let Some(entry) = self.redo.pop() {
            self.discard_entry(entry);
        }
    }

    fn outcome(&self, document_changed: bool, history_changed: bool) -> NativeEditOutcome {
        NativeEditOutcome {
            revision: self.session.revision(),
            selection: self.selection.clone(),
            document_changed,
            history_changed,
            composition_id: self.composition_id(),
        }
    }
}

/// Containers: a position's paragraph list is the section body when its cell
/// path is empty, else the paragraphs of the innermost addressed cell. Edits
/// dispatch to the core's body or `_by_path` cell methods accordingly.
impl NativeEditor {
    /// The position's paragraph index within its container.
    pub(super) fn leaf(position: &NativeTextPosition) -> u32 {
        position
            .cell_path
            .last()
            .map_or(position.paragraph, |entry| entry.paragraph)
    }

    /// `position` moved to `offset` in paragraph `paragraph` of the same container.
    pub(super) fn at_leaf(
        position: &NativeTextPosition,
        paragraph: u32,
        offset: u32,
    ) -> NativeTextPosition {
        let mut moved = position.clone();
        match moved.cell_path.last_mut() {
            Some(entry) => entry.paragraph = paragraph,
            None => moved.paragraph = paragraph,
        }
        moved.scalar_offset = offset;
        moved
    }

    pub(super) fn same_container(a: &NativeTextPosition, b: &NativeTextPosition) -> bool {
        a.section == b.section
            && match (a.cell_path.split_last(), b.cell_path.split_last()) {
                (None, None) => true,
                (Some((a_leaf, a_prefix)), Some((b_leaf, b_prefix))) => {
                    a.paragraph == b.paragraph
                        && a_prefix == b_prefix
                        && (a_leaf.control, a_leaf.cell) == (b_leaf.control, b_leaf.cell)
                }
                _ => false,
            }
    }

    /// The paragraphs of the container holding `position`.
    pub(super) fn container<'a>(
        core: &'a DocumentCore,
        position: &NativeTextPosition,
    ) -> Result<&'a [Paragraph], NativeHostError> {
        let outside =
            || NativeHostError::InvalidArgument("text position is outside the document".into());
        let mut paragraphs: &[Paragraph] = &core
            .document()
            .sections
            .get(position.section as usize)
            .ok_or_else(outside)?
            .paragraphs;
        let mut index = position.paragraph as usize;
        for entry in &position.cell_path {
            let Some(Control::Table(table)) = paragraphs
                .get(index)
                .and_then(|paragraph| paragraph.controls.get(entry.control as usize))
            else {
                return Err(outside());
            };
            paragraphs = &table
                .cells
                .get(entry.cell as usize)
                .ok_or_else(outside)?
                .paragraphs;
            index = entry.paragraph as usize;
        }
        Ok(paragraphs)
    }

    /// The position's paragraph.
    pub(super) fn paragraph_at<'a>(
        core: &'a DocumentCore,
        position: &NativeTextPosition,
    ) -> Result<&'a Paragraph, NativeHostError> {
        Self::container(core, position)?
            .get(Self::leaf(position) as usize)
            .ok_or_else(|| {
                NativeHostError::InvalidArgument("text position is outside the document".into())
            })
    }

    /// Text of the position's paragraph on the native axis (inline objects
    /// as U+FFFC).
    pub(super) fn paragraph_text<'a>(
        core: &'a DocumentCore,
        position: &NativeTextPosition,
    ) -> Result<Cow<'a, str>, NativeHostError> {
        Ok(native_paragraph_text(Self::paragraph_at(core, position)?))
    }

    /// The position as an index into its paragraph's text, the axis of the
    /// core's character-indexed methods (formatting, properties, inserting
    /// controls). The same offset in a paragraph without inline objects.
    pub(super) fn text_offset(
        core: &DocumentCore,
        position: &NativeTextPosition,
    ) -> Result<usize, NativeHostError> {
        Ok(native_to_text_offset(
            Self::paragraph_at(core, position)?,
            position.scalar_offset as usize,
        )
        .0)
    }

    pub(super) fn holds_inline(
        core: &DocumentCore,
        start: &NativeTextPosition,
        end: &NativeTextPosition,
    ) -> bool {
        Self::container(core, start).is_ok_and(|paragraphs| {
            paragraphs
                .get(Self::leaf(start) as usize..=Self::leaf(end) as usize)
                .is_some_and(|range| {
                    range
                        .iter()
                        .any(|paragraph| paragraph.controls.iter().any(Control::is_logical_inline))
                })
        })
    }

    /// The core's `_by_path` address: (control, cell, cell paragraph) per level.
    pub(super) fn core_path(position: &NativeTextPosition) -> Vec<(usize, usize, usize)> {
        position
            .cell_path
            .iter()
            .map(|entry| {
                (
                    entry.control as usize,
                    entry.cell as usize,
                    entry.paragraph as usize,
                )
            })
            .collect()
    }

    /// Inserts at a logical caret: next to an inline object, the text goes
    /// on the caret's side of it.
    pub(super) fn insert_in_container(
        core: &mut DocumentCore,
        position: &NativeTextPosition,
        text: &str,
    ) -> Result<(), NativeHostError> {
        let (offset, after_object) = native_to_text_offset(
            Self::paragraph_at(core, position)?,
            position.scalar_offset as usize,
        );
        // Text typed at a 누름틀 goes inside it, as in 한글: the core keeps text
        // in its active field. Any other position has none.
        core.clear_active_field();
        if position.cell_path.is_empty() {
            core.set_active_field(
                position.section as usize,
                position.paragraph as usize,
                offset,
            );
        } else {
            core.set_active_field_by_path(
                position.section as usize,
                position.paragraph as usize,
                &Self::core_path(position),
                offset,
            );
        }
        if position.cell_path.is_empty() {
            core.insert_text_at_caret_native(
                position.section as usize,
                position.paragraph as usize,
                offset,
                after_object,
                text,
            )?;
        } else {
            core.insert_text_in_cell_at_caret_by_path(
                position.section as usize,
                position.paragraph as usize,
                &Self::core_path(position),
                offset,
                after_object,
                text,
            )?;
        }
        Ok(())
    }

    pub(super) fn delete_in_container(
        core: &mut DocumentCore,
        start: &NativeTextPosition,
        end: &NativeTextPosition,
    ) -> Result<(), NativeHostError> {
        let inline = Self::holds_inline(core, start, end);
        if start.cell_path.is_empty() && inline {
            core.delete_range_logical_native(
                start.section as usize,
                start.paragraph as usize,
                start.scalar_offset as usize,
                end.paragraph as usize,
                end.scalar_offset as usize,
            )?;
        } else if start.cell_path.is_empty() {
            core.delete_range_native(
                start.section as usize,
                start.paragraph as usize,
                start.scalar_offset as usize,
                end.paragraph as usize,
                end.scalar_offset as usize,
                None,
            )?;
        } else {
            // Cells take text indices; the range holds no inline object (see
            // `check_body_text_span`), so its ends map to the same characters.
            let (from, to) = (
                Self::text_offset(core, start)?,
                Self::text_offset(core, end)?,
            );
            core.delete_range_in_cell_by_path(
                start.section as usize,
                start.paragraph as usize,
                &Self::core_path(start),
                Self::leaf(start) as usize,
                from,
                Self::leaf(end) as usize,
                to,
            )?;
        }
        Ok(())
    }

    /// 양식 모드 lets text change only inside one 누름틀 that is editable in
    /// forms; `None` (paragraph splits, joins, actions) is refused outright.
    pub(super) fn check_form_mode(
        core: &DocumentCore,
        selection: Option<&NativeTextSelection>,
    ) -> Result<(), NativeHostError> {
        if !core.native_form_mode {
            return Ok(());
        }
        let refused = || {
            NativeHostError::Unsupported(
                "in form mode only the text of a click-here field can change".into(),
            )
        };
        let selection = selection.ok_or_else(refused)?;
        let (start, end) = Self::ordered(selection);
        if !Self::same_container(start, end) || Self::leaf(start) != Self::leaf(end) {
            return Err(refused());
        }
        let (from, to) = (
            Self::text_offset(core, start)?,
            Self::text_offset(core, end)?,
        );
        let info = if start.cell_path.is_empty() {
            core.get_field_info_at(start.section as usize, start.paragraph as usize, from)
        } else {
            core.get_field_info_at_by_path(
                start.section as usize,
                start.paragraph as usize,
                &Self::core_path(start),
                from,
            )
        };
        let info: serde_json::Value = serde_json::from_str(&info).unwrap_or_default();
        let inside = info["inField"] == true
            && info["editableInForm"] == true
            && info["startCharIdx"]
                .as_u64()
                .is_some_and(|first| first as usize <= from)
            && info["endCharIdx"]
                .as_u64()
                .is_some_and(|last| to <= last as usize);
        if inside {
            Ok(())
        } else {
            Err(refused())
        }
    }

    /// Splits the position's paragraph at its offset.
    pub(super) fn split_in_container(
        core: &mut DocumentCore,
        position: &NativeTextPosition,
    ) -> Result<(), NativeHostError> {
        Self::check_form_mode(core, None)?;
        if position.cell_path.is_empty() {
            core.split_paragraph_native(
                position.section as usize,
                position.paragraph as usize,
                position.scalar_offset as usize,
                None,
            )?;
        } else {
            core.split_paragraph_in_cell_by_path(
                position.section as usize,
                position.paragraph as usize,
                &Self::core_path(position),
                position.scalar_offset as usize,
                None,
            )?;
        }
        Ok(())
    }

    /// 한글 recalculates a table's 계산식 whenever one of its cells changes;
    /// so does this, for top-level tables after a cell text edit.
    pub(super) fn recalculate_formulas(
        core: &mut DocumentCore,
        position: &NativeTextPosition,
    ) -> Result<(), NativeHostError> {
        if let [cell] = position.cell_path.as_slice() {
            core.recalculate_table_formulas(
                position.section as usize,
                position.paragraph as usize,
                cell.control as usize,
                Some(cell.cell as usize),
                true,
            )?;
        }
        Ok(())
    }

    /// Merges the position's paragraph into the previous one.
    pub(super) fn merge_in_container(
        core: &mut DocumentCore,
        position: &NativeTextPosition,
    ) -> Result<(), NativeHostError> {
        Self::check_form_mode(core, None)?;
        if position.cell_path.is_empty() {
            core.merge_paragraph_native(position.section as usize, position.paragraph as usize)?;
        } else {
            core.merge_paragraph_in_cell_by_path(
                position.section as usize,
                position.paragraph as usize,
                &Self::core_path(position),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod cell_tests {
    use super::actions::{NativeAction, NativeQuery};
    use super::*;
    use crate::document_core::native_interaction::NativeCellPathEntry;
    use crate::native_host::commands::NativeEditCommand;
    use serde_json::json;

    /// A blank document holding a 2x2 table; returns the editor and the
    /// body paragraph and control index of the table.
    fn table_editor() -> (NativeEditor, u32, u32) {
        let mut core = DocumentCore::new_empty();
        core.create_blank_document_native().unwrap();
        let created: serde_json::Value =
            serde_json::from_str(&core.create_table_native(0, 0, 0, 2, 2).unwrap()).unwrap();
        let paragraph = created["paraIdx"].as_u64().unwrap() as u32;
        let control = created["controlIdx"].as_u64().unwrap() as u32;
        let start = NativeTextPosition {
            section: 0,
            paragraph: 0,
            scalar_offset: 0,
            cell_path: Vec::new(),
        };
        let editor = NativeEditor::new(
            NativeDocumentSession::from_core(core),
            NativeTextSelection {
                anchor: start.clone(),
                focus: start,
            },
        )
        .unwrap();
        (editor, paragraph, control)
    }

    fn in_cell(
        paragraph: u32,
        control: u32,
        cell: u32,
        cell_paragraph: u32,
        offset: u32,
    ) -> NativeTextPosition {
        NativeTextPosition {
            section: 0,
            paragraph,
            scalar_offset: offset,
            cell_path: vec![NativeCellPathEntry {
                control,
                cell,
                paragraph: cell_paragraph,
            }],
        }
    }

    fn caret(editor: &mut NativeEditor, position: NativeTextPosition) {
        let revision = editor.session().revision();
        editor
            .set_selection(
                revision,
                NativeTextSelection {
                    anchor: position.clone(),
                    focus: position,
                },
            )
            .unwrap();
    }

    fn type_text(editor: &mut NativeEditor, text: &str) {
        let revision = editor.session().revision();
        editor.replace_selection(revision, text).unwrap();
    }

    fn command(editor: &mut NativeEditor, command: serde_json::Value) {
        let revision = editor.session().revision();
        let command: NativeEditCommand = serde_json::from_value(command).unwrap();
        editor.execute_command(revision, command).unwrap();
    }

    fn cell_texts(editor: &NativeEditor, paragraph: u32, control: u32, cell: u32) -> Vec<String> {
        let position = in_cell(paragraph, control, cell, 0, 0);
        NativeEditor::container(editor.session().document(), &position)
            .unwrap()
            .iter()
            .map(|paragraph| paragraph.text.clone())
            .collect()
    }

    #[test]
    fn typing_splitting_and_joining_stay_inside_the_cell() {
        let (mut editor, paragraph, control) = table_editor();
        caret(&mut editor, in_cell(paragraph, control, 1, 0, 0));
        type_text(&mut editor, "가나");
        command(&mut editor, json!({"command": "splitParagraph"}));
        type_text(&mut editor, "다");
        assert_eq!(cell_texts(&editor, paragraph, control, 1), ["가나", "다"]);
        assert_eq!(cell_texts(&editor, paragraph, control, 0), [""]);
        assert_eq!(
            editor.selection().focus,
            in_cell(paragraph, control, 1, 1, 1)
        );
        command(&mut editor, json!({"command": "moveLeft"}));
        command(&mut editor, json!({"command": "deleteBackward"}));
        assert_eq!(cell_texts(&editor, paragraph, control, 1), ["가나다"]);
        assert_eq!(
            editor.selection().focus,
            in_cell(paragraph, control, 1, 0, 2)
        );
        // Backspace at the start of a cell does not leave it.
        caret(&mut editor, in_cell(paragraph, control, 1, 0, 0));
        command(&mut editor, json!({"command": "deleteBackward"}));
        assert_eq!(cell_texts(&editor, paragraph, control, 1), ["가나다"]);
        let revision = editor.session().revision();
        editor.undo(revision).unwrap();
        assert_eq!(cell_texts(&editor, paragraph, control, 1), ["가나", "다"]);
    }

    #[test]
    fn select_all_in_a_cell_selects_the_cell_and_cut_empties_it() {
        let (mut editor, paragraph, control) = table_editor();
        caret(&mut editor, in_cell(paragraph, control, 2, 0, 0));
        type_text(&mut editor, "셀");
        command(&mut editor, json!({"command": "selectAll"}));
        let revision = editor.session().revision();
        let (payload, _) = editor.cut_selection(revision).unwrap();
        assert_eq!(payload.plain_text, "셀");
        assert_eq!(cell_texts(&editor, paragraph, control, 2), [""]);
        let revision = editor.session().revision();
        editor.paste_text(revision, "첫\n둘").unwrap();
        assert_eq!(cell_texts(&editor, paragraph, control, 2), ["첫", "둘"]);
    }

    #[test]
    fn a_selection_cannot_cross_cells() {
        let (mut editor, paragraph, control) = table_editor();
        let revision = editor.session().revision();
        let error = editor
            .set_selection(
                revision,
                NativeTextSelection {
                    anchor: in_cell(paragraph, control, 0, 0, 0),
                    focus: in_cell(paragraph, control, 1, 0, 0),
                },
            )
            .unwrap_err();
        assert!(matches!(error, NativeHostError::Unsupported(_)));
    }

    #[test]
    fn formatting_and_find_work_in_cells() {
        let (mut editor, paragraph, control) = table_editor();
        caret(&mut editor, in_cell(paragraph, control, 3, 0, 0));
        type_text(&mut editor, "표 안 글자");
        let revision = editor.session().revision();
        editor
            .set_selection(
                revision,
                NativeTextSelection {
                    anchor: in_cell(paragraph, control, 3, 0, 0),
                    focus: in_cell(paragraph, control, 3, 0, 1),
                },
            )
            .unwrap();
        let revision = editor.session().revision();
        let action: NativeAction =
            serde_json::from_value(json!({"action": "charFormat", "props": {"bold": true}}))
                .unwrap();
        editor.apply_action(revision, action).unwrap();
        let action: NativeAction = serde_json::from_value(
            json!({"action": "paraFormat", "props": {"alignment": "center"}}),
        )
        .unwrap();
        let revision = editor.session().revision();
        editor.apply_action(revision, action).unwrap();
        assert_eq!(
            editor.query(NativeQuery::CharProperties {}).unwrap()["bold"],
            json!(true)
        );
        assert_eq!(
            editor.query(NativeQuery::ParaProperties {}).unwrap()["alignment"],
            json!("center")
        );

        caret(
            &mut editor,
            NativeTextPosition {
                section: 0,
                paragraph: 0,
                scalar_offset: 0,
                cell_path: Vec::new(),
            },
        );
        let hit = editor
            .query(NativeQuery::Find {
                query: "글자".into(),
                forward: true,
                case_sensitive: true,
            })
            .unwrap();
        assert_eq!(hit["found"], json!(true));
        let position: NativeTextPosition = serde_json::from_value(hit["position"].clone()).unwrap();
        assert_eq!(position, in_cell(paragraph, control, 3, 0, 4));
    }

    fn act(editor: &mut NativeEditor, action: serde_json::Value) {
        let revision = editor.session().revision();
        let action: NativeAction = serde_json::from_value(action).unwrap();
        editor.apply_action(revision, action).unwrap();
    }

    fn shape(editor: &NativeEditor, paragraph: u32, control: u32) -> (u16, u16, usize) {
        match &editor.session().document().document().sections[0].paragraphs[paragraph as usize]
            .controls[control as usize]
        {
            Control::Table(table) => (table.row_count, table.col_count, table.cells.len()),
            _ => panic!("not a table"),
        }
    }

    #[test]
    fn insert_table_puts_the_caret_in_its_first_cell() {
        let mut core = DocumentCore::new_empty();
        core.create_blank_document_native().unwrap();
        let start = NativeTextPosition {
            section: 0,
            paragraph: 0,
            scalar_offset: 0,
            cell_path: Vec::new(),
        };
        let mut editor = NativeEditor::new(
            NativeDocumentSession::from_core(core),
            NativeTextSelection {
                anchor: start.clone(),
                focus: start,
            },
        )
        .unwrap();
        act(
            &mut editor,
            json!({"action": "insertTable", "rows": 3, "cols": 2}),
        );
        let focus = editor.selection().focus.clone();
        assert_eq!(focus.cell_path.len(), 1);
        assert_eq!(focus.cell_path[0].cell, 0);
        let (paragraph, control) = (focus.paragraph, focus.cell_path[0].control);
        assert_eq!(shape(&editor, paragraph, control), (3, 2, 6));
        type_text(&mut editor, "첫 칸");
        assert_eq!(cell_texts(&editor, paragraph, control, 0), ["첫 칸"]);
        let revision = editor.session().revision();
        editor.undo(revision).unwrap();
        editor.undo(editor.session().revision()).unwrap();
        assert!(!editor.session().document().document().sections[0]
            .paragraphs
            .iter()
            .any(|paragraph| paragraph
                .controls
                .iter()
                .any(|control| matches!(control, Control::Table(_)))));
    }

    #[test]
    fn rows_and_columns_insert_and_delete_around_the_caret() {
        let (mut editor, paragraph, control) = table_editor();
        // Cell 3 is row 1, column 1 of the 2x2 table.
        caret(&mut editor, in_cell(paragraph, control, 3, 0, 0));
        type_text(&mut editor, "끝");
        act(&mut editor, json!({"action": "insertRow", "below": false}));
        assert_eq!(shape(&editor, paragraph, control), (3, 2, 6));
        act(
            &mut editor,
            json!({"action": "insertColumn", "right": true}),
        );
        assert_eq!(shape(&editor, paragraph, control), (3, 3, 9));
        // The caret stays in the cell that holds the text.
        let focus = editor.selection().focus.clone();
        assert_eq!(
            cell_texts(&editor, paragraph, control, focus.cell_path[0].cell),
            ["끝"]
        );
        act(&mut editor, json!({"action": "deleteRow"}));
        act(&mut editor, json!({"action": "deleteColumn"}));
        assert_eq!(shape(&editor, paragraph, control), (2, 2, 4));
        act(
            &mut editor,
            json!({"action": "insertRow", "below": true, "count": 3}),
        );
        assert_eq!(shape(&editor, paragraph, control), (5, 2, 10));
        let revision = editor.session().revision();
        editor.undo(revision).unwrap();
        assert_eq!(shape(&editor, paragraph, control), (2, 2, 4));
    }

    #[test]
    fn merge_split_and_delete_table() {
        let (mut editor, paragraph, control) = table_editor();
        caret(&mut editor, in_cell(paragraph, control, 0, 0, 0));
        act(&mut editor, json!({"action": "mergeRight"}));
        assert_eq!(shape(&editor, paragraph, control).2, 3);
        // The merged cell spans both columns, so merging down takes both cells
        // below it: the merge stays a rectangle.
        act(&mut editor, json!({"action": "mergeDown"}));
        assert_eq!(shape(&editor, paragraph, control).2, 1);
        act(
            &mut editor,
            json!({"action": "splitCell", "rows": 1, "cols": 3}),
        );
        assert!(shape(&editor, paragraph, control).2 > 2);
        act(&mut editor, json!({"action": "deleteTable"}));
        assert!(editor.selection().focus.cell_path.is_empty());
        assert!(
            editor.session().document().document().sections[0].paragraphs[paragraph as usize]
                .controls
                .get(control as usize)
                .is_none_or(|control| !matches!(control, Control::Table(_)))
        );
    }

    #[test]
    fn tab_moves_between_cells_and_adds_a_row_after_the_last() {
        let (mut editor, paragraph, control) = table_editor();
        caret(&mut editor, in_cell(paragraph, control, 2, 0, 0));
        command(&mut editor, json!({"command": "nextCell"}));
        assert_eq!(
            editor.selection().focus,
            in_cell(paragraph, control, 3, 0, 0)
        );
        command(&mut editor, json!({"command": "previousCell"}));
        assert_eq!(
            editor.selection().focus,
            in_cell(paragraph, control, 2, 0, 0)
        );
        caret(&mut editor, in_cell(paragraph, control, 0, 0, 0));
        command(&mut editor, json!({"command": "previousCell"}));
        assert_eq!(
            editor.selection().focus,
            in_cell(paragraph, control, 0, 0, 0)
        );
        caret(&mut editor, in_cell(paragraph, control, 3, 0, 0));
        command(&mut editor, json!({"command": "nextCell"}));
        assert_eq!(shape(&editor, paragraph, control), (3, 2, 6));
        assert_eq!(
            editor.selection().focus,
            in_cell(paragraph, control, 4, 0, 0)
        );
    }

    #[test]
    fn saving_as_hwp_with_the_caret_in_a_cell_works() {
        let (mut editor, paragraph, control) = table_editor();
        caret(&mut editor, in_cell(paragraph, control, 1, 0, 0));
        type_text(&mut editor, "셀");
        for format in [
            crate::native_host::save::NativeSaveFormat::Hwp,
            crate::native_host::save::NativeSaveFormat::Hwpx,
        ] {
            let prepared = editor.prepare_save(format).unwrap();
            editor.cancel_save(prepared.token()).unwrap();
        }
    }

    #[test]
    fn split_table_and_attach_it_again() {
        let mut core = DocumentCore::new_empty();
        core.create_blank_document_native().unwrap();
        let created: serde_json::Value =
            serde_json::from_str(&core.create_table_native(0, 0, 0, 3, 2).unwrap()).unwrap();
        let paragraph = created["paraIdx"].as_u64().unwrap() as u32;
        let control = created["controlIdx"].as_u64().unwrap() as u32;
        let start = in_cell(paragraph, control, 0, 0, 0);
        let mut editor = NativeEditor::new(
            NativeDocumentSession::from_core(core),
            NativeTextSelection {
                anchor: start.clone(),
                focus: start,
            },
        )
        .unwrap();
        // Row 1, column 1: cell 3 of the 3x2 table.
        caret(&mut editor, in_cell(paragraph, control, 3, 0, 0));
        act(&mut editor, json!({"action": "splitTable"}));
        assert_eq!(shape(&editor, paragraph, control), (1, 2, 2));
        let focus = editor.selection().focus.clone();
        assert_eq!(focus.paragraph, paragraph + 2);
        let below = (focus.paragraph, focus.cell_path[0].control);
        assert_eq!(shape(&editor, below.0, below.1), (2, 2, 4));
        assert_eq!(focus.cell_path[0].cell, 1);
        caret(&mut editor, in_cell(paragraph, control, 0, 0, 0));
        act(&mut editor, json!({"action": "attachNextTable"}));
        assert_eq!(shape(&editor, paragraph, control), (3, 2, 6));
    }

    #[test]
    fn cell_formula_sums_the_cells_above_and_formats_numbers() {
        let mut core = DocumentCore::new_empty();
        core.create_blank_document_native().unwrap();
        let created: serde_json::Value =
            serde_json::from_str(&core.create_table_native(0, 0, 0, 3, 1).unwrap()).unwrap();
        let paragraph = created["paraIdx"].as_u64().unwrap() as u32;
        let control = created["controlIdx"].as_u64().unwrap() as u32;
        let start = in_cell(paragraph, control, 0, 0, 0);
        let mut editor = NativeEditor::new(
            NativeDocumentSession::from_core(core),
            NativeTextSelection {
                anchor: start.clone(),
                focus: start,
            },
        )
        .unwrap();
        type_text(&mut editor, "1200");
        caret(&mut editor, in_cell(paragraph, control, 1, 0, 0));
        type_text(&mut editor, "34");
        caret(&mut editor, in_cell(paragraph, control, 2, 0, 0));
        act(
            &mut editor,
            json!({"action": "setCellFormula", "formula": "=SUM(ABOVE)", "format": "%g"}),
        );
        assert_eq!(cell_texts(&editor, paragraph, control, 2), ["1234"]);
        // Editing a number the formula reads recalculates it, as in 한글.
        caret(&mut editor, in_cell(paragraph, control, 1, 0, 2));
        type_text(&mut editor, "0");
        assert_eq!(cell_texts(&editor, paragraph, control, 2), ["1540"]);
        caret(&mut editor, in_cell(paragraph, control, 0, 0, 0));
        act(
            &mut editor,
            json!({"action": "formatCellNumber", "operation": "thousands"}),
        );
        assert_eq!(cell_texts(&editor, paragraph, control, 0), ["1,200"]);
        act(
            &mut editor,
            json!({"action": "formatCellNumber", "operation": "addDecimal"}),
        );
        assert_eq!(cell_texts(&editor, paragraph, control, 0), ["1,200.0"]);
    }
}

#[cfg(test)]
mod object_paragraph_tests {
    use super::*;

    /// 1×1 PNG.
    const PNG: [u8; 67] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn at(offset: u32) -> NativeTextPosition {
        NativeTextPosition {
            section: 0,
            paragraph: 0,
            scalar_offset: offset,
            cell_path: Vec::new(),
        }
    }

    fn range(anchor: u32, focus: u32) -> NativeTextSelection {
        NativeTextSelection {
            anchor: at(anchor),
            focus: at(focus),
        }
    }

    /// "가나다라" with a floating picture anchored at offset 2.
    fn editor_with_picture() -> NativeEditor {
        let mut core = DocumentCore::new_empty();
        core.create_blank_document_native().unwrap();
        core.insert_text_native(0, 0, 0, "가나다라").unwrap();
        core.insert_picture_native(0, 0, 2, &[], &PNG, 7500, 7500, 1, 1, "png", "", None, None)
            .unwrap();
        NativeEditor::new(NativeDocumentSession::from_core(core), range(0, 0)).unwrap()
    }

    fn text(editor: &NativeEditor) -> &str {
        &editor.session().document().document().sections[0].paragraphs[0].text
    }

    fn has_picture(editor: &NativeEditor) -> bool {
        editor.session().document().document().sections[0].paragraphs[0]
            .controls
            .iter()
            .any(|control| matches!(control, Control::Picture(_)))
    }

    fn replace(
        editor: &mut NativeEditor,
        selection: NativeTextSelection,
        text: &str,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        let revision = editor.session().revision();
        editor.set_selection(revision, selection)?;
        let revision = editor.session().revision();
        editor.replace_selection(revision, text)
    }

    #[test]
    fn typing_and_deleting_around_a_floating_picture_keeps_it() {
        let mut editor = editor_with_picture();
        replace(&mut editor, range(4, 4), "마").unwrap();
        replace(&mut editor, range(0, 1), "").unwrap();
        // The picture sits before 다 (offset 1 now); deleting up to it keeps it.
        replace(&mut editor, range(0, 1), "").unwrap();
        assert_eq!(text(&editor), "다라마");
        assert!(has_picture(&editor));
    }

    #[test]
    fn deleting_across_the_picture_anchor_is_refused_without_change() {
        let mut editor = editor_with_picture();
        let revision = editor.session().revision();
        let error = replace(&mut editor, range(1, 3), "").unwrap_err();
        assert!(matches!(error, NativeHostError::Unsupported(_)));
        assert_eq!(text(&editor), "가나다라");
        assert!(has_picture(&editor));
        assert_eq!(editor.session().revision(), revision);
    }
}

#[cfg(test)]
mod marks_tests {
    use super::*;
    use crate::native_host::NativeEditorMarks;

    #[test]
    fn editor_marks_change_neither_document_nor_revision() {
        let mut core = DocumentCore::new_empty();
        core.create_blank_document_native().unwrap();
        core.insert_text_native(0, 0, 0, "문단").unwrap();
        let start = NativeTextPosition {
            section: 0,
            paragraph: 0,
            scalar_offset: 0,
            cell_path: Vec::new(),
        };
        let mut editor = NativeEditor::new(
            NativeDocumentSession::from_core(core),
            NativeTextSelection {
                anchor: start.clone(),
                focus: start,
            },
        )
        .unwrap();
        let revision = editor.session().revision();
        let before = format!("{:?}", editor.session().document().document());
        let marks = NativeEditorMarks {
            paragraph_marks: true,
            control_codes: false,
            transparent_borders: true,
            clip: false,
            form_mode: false,
        };
        editor.set_editor_marks(marks);
        assert_eq!(editor.session().editor_marks(), marks);
        assert_eq!(editor.session().revision(), revision);
        assert_eq!(
            format!("{:?}", editor.session().document().document()),
            before
        );
        assert!(!editor.can_undo());
    }
}

#[cfg(test)]
mod inline_object_tests {
    use super::actions::NativeQuery;
    use super::*;
    use crate::document_core::native_interaction::native_paragraph_text;
    use serde_json::json;

    const PNG: [u8; 67] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn at(offset: u32) -> NativeTextPosition {
        NativeTextPosition {
            section: 0,
            paragraph: 0,
            scalar_offset: offset,
            cell_path: Vec::new(),
        }
    }

    fn range(anchor: u32, focus: u32) -> NativeTextSelection {
        NativeTextSelection {
            anchor: at(anchor),
            focus: at(focus),
        }
    }

    /// "가나다라" with a treat-as-char picture between 나 and 다: native
    /// offsets 가0 나1 [그림]2 다3 라4.
    fn editor_with_inline_picture() -> NativeEditor {
        let mut core = DocumentCore::new_empty();
        core.create_blank_document_native().unwrap();
        core.insert_text_native(0, 0, 0, "가나다라").unwrap();
        core.insert_picture_native(0, 0, 2, &[], &PNG, 7500, 7500, 1, 1, "png", "", None, None)
            .unwrap();
        let mut document = core.document().clone();
        for control in &mut document.sections[0].paragraphs[0].controls {
            if let Control::Picture(picture) = control {
                picture.common.treat_as_char = true;
            }
        }
        core.set_document(document);
        NativeEditor::new(NativeDocumentSession::from_core(core), range(0, 0)).unwrap()
    }

    fn paragraph(editor: &NativeEditor) -> &Paragraph {
        &editor.session().document().document().sections[0].paragraphs[0]
    }

    fn shown(editor: &NativeEditor) -> String {
        native_paragraph_text(paragraph(editor)).into_owned()
    }

    fn has_picture(editor: &NativeEditor) -> bool {
        paragraph(editor)
            .controls
            .iter()
            .any(|control| matches!(control, Control::Picture(_)))
    }

    fn replace(
        editor: &mut NativeEditor,
        selection: NativeTextSelection,
        text: &str,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        let revision = editor.session().revision();
        editor.set_selection(revision, selection)?;
        let revision = editor.session().revision();
        editor.replace_selection(revision, text)
    }

    #[test]
    fn an_inline_object_takes_one_offset_and_shows_as_object_replacement() {
        let editor = editor_with_inline_picture();
        assert_eq!(shown(&editor), "가나\u{FFFC}다라");
        let core = editor.session().document();
        let carets: Vec<f64> = (0..=5)
            .map(|offset| core.native_caret_rect(&at(offset)).unwrap().x)
            .collect();
        assert!(
            carets.windows(2).all(|pair| pair[0] < pair[1]),
            "{carets:?}"
        );
        assert!(core.native_caret_rect(&at(6)).is_err());
    }

    #[test]
    fn typing_goes_on_the_caret_side_of_the_inline_object() {
        let mut editor = editor_with_inline_picture();
        replace(&mut editor, range(2, 2), "앞").unwrap();
        assert_eq!(shown(&editor), "가나앞\u{FFFC}다라");
        replace(&mut editor, range(4, 4), "뒤").unwrap();
        assert_eq!(shown(&editor), "가나앞\u{FFFC}뒤다라");
        assert_eq!(paragraph(&editor).text, "가나앞뒤다라");
        assert_eq!(editor.selection().focus.scalar_offset, 5);
        // A long insertion takes the general path, not the local fast path.
        replace(&mut editor, range(5, 5), "0123456789").unwrap();
        assert_eq!(shown(&editor), "가나앞\u{FFFC}뒤0123456789다라");
        assert!(has_picture(&editor));
    }

    #[test]
    fn deleting_text_keeps_the_object_and_deleting_the_object_is_refused() {
        let mut editor = editor_with_inline_picture();
        // Delete 나 (before) and 다 (after) the picture.
        replace(&mut editor, range(1, 2), "").unwrap();
        assert_eq!(shown(&editor), "가\u{FFFC}다라");
        replace(&mut editor, range(2, 3), "").unwrap();
        assert_eq!(shown(&editor), "가\u{FFFC}라");
        let revision = editor.session().revision();
        for selection in [range(1, 2), range(0, 3), range(2, 1)] {
            assert!(matches!(
                replace(&mut editor, selection, "x"),
                Err(NativeHostError::Unsupported(_))
            ));
        }
        assert_eq!(editor.session().revision(), revision);
        assert_eq!(shown(&editor), "가\u{FFFC}라");
        assert!(has_picture(&editor));
    }

    #[test]
    fn formatting_and_properties_index_the_characters_around_the_object() {
        let mut editor = editor_with_inline_picture();
        let revision = editor.session().revision();
        editor.set_selection(revision, range(3, 5)).unwrap();
        let revision = editor.session().revision();
        editor
            .apply_action(
                revision,
                serde_json::from_value(json!({"action": "charFormat", "props": {"bold": true}}))
                    .unwrap(),
            )
            .unwrap();
        let bold = |editor: &mut NativeEditor, offset: u32| {
            let revision = editor.session().revision();
            editor
                .set_selection(revision, range(offset, offset + 1))
                .unwrap();
            editor.query(NativeQuery::CharProperties {}).unwrap()["bold"].clone()
        };
        assert_eq!(bold(&mut editor, 3), json!(true));
        assert_eq!(bold(&mut editor, 4), json!(true));
        assert_eq!(bold(&mut editor, 1), json!(false));
    }

    /// Text typed before a treat-as-char table shares its line. In this sample
    /// the layout puts those runs outside a text line; only the complete page
    /// text (search indexing) picks them up.
    #[test]
    fn complete_page_text_keeps_the_text_beside_an_inline_table() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/samples/2010-01-06.hwp");
        let core = DocumentCore::from_bytes(&std::fs::read(path).unwrap()).unwrap();
        assert!(paragraph_holds_inline_table(
            &core.document().sections[0].paragraphs[0]
        ));
        let start = NativeTextPosition {
            section: 0,
            paragraph: 0,
            scalar_offset: 0,
            cell_path: Vec::new(),
        };
        let mut editor = NativeEditor::new(
            NativeDocumentSession::from_core(core),
            NativeTextSelection {
                anchor: start.clone(),
                focus: start,
            },
        )
        .unwrap();
        editor.replace_selection(0, "새 문단").unwrap();
        let core = editor.session().document();
        assert!(!core
            .extract_page_text_native(0)
            .unwrap()
            .contains("새 문단"));
        assert!(core
            .extract_page_text_complete_native(0)
            .unwrap()
            .contains("새 문단"));
    }

    fn paragraph_holds_inline_table(paragraph: &Paragraph) -> bool {
        paragraph.controls.iter().any(|control| match control {
            Control::Table(table) => table.common.treat_as_char,
            _ => false,
        })
    }

    #[test]
    fn splitting_after_the_object_keeps_it_in_the_first_paragraph() {
        let mut editor = editor_with_inline_picture();
        let revision = editor.session().revision();
        editor.set_selection(revision, range(3, 3)).unwrap();
        let revision = editor.session().revision();
        editor
            .execute_command(
                revision,
                serde_json::from_value(json!({"command": "splitParagraph"})).unwrap(),
            )
            .unwrap();
        let paragraphs = &editor.session().document().document().sections[0].paragraphs;
        assert_eq!(native_paragraph_text(&paragraphs[0]), "가나\u{FFFC}");
        assert_eq!(native_paragraph_text(&paragraphs[1]), "다라");
    }
}
