//! Formatting and document-wide actions for native hosts.
//!
//! These wrap the `DocumentCore::*_native` methods that the web editor reaches
//! through `wasm_api`, so a native host can offer the same features. Every
//! mutating action is one undo entry. Property JSON uses the same keys as the
//! web editor (`{"bold":true}`, `{"fontSize":1200}`, `{"alignment":"center"}`,
//! `{"textColor":"#ff0000"}`, ...); `fontName` is resolved to a `fontId`.
//! Like the rest of this surface, actions apply to body or table-cell text.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::document_core::native_interaction::{
    NativeCellPathEntry, NativeTextPosition, NativeTextSelection,
};
use crate::model::control::Control;
use crate::model::table::Table;
use crate::DocumentCore;

use super::{NativeEditOutcome, NativeEditor, NativeHostError};

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum NativeAction {
    /// Character properties for the selected text. Requires a selection.
    CharFormat {
        props: Value,
    },
    /// Paragraph properties for every paragraph the selection touches.
    ParaFormat {
        props: Value,
    },
    /// Numbering (1. 가) …) on or off for the selected paragraphs, by the
    /// state of the first one; uses the document's first numbering definition.
    ToggleNumbering {},
    /// A page (column) break at the caret: the paragraph splits there and the
    /// second part starts a new page (column). Body text only.
    InsertPageBreak {},
    InsertColumnBreak {},
    /// Page setup of the caret's section, as rhwp's PageDef JSON (HWPUNIT).
    SetPageDef {
        props: Value,
    },
    /// Replaces the text of a header or footer of the caret's section,
    /// creating it if needed. `apply_to`: 0 both, 1 even, 2 odd pages. In
    /// `text`, newlines separate paragraphs and U+0015, U+0016, U+0017 are
    /// the page number, page count and file name fields.
    SetHeaderFooter {
        is_header: bool,
        apply_to: u8,
        text: String,
    },
    DeleteHeaderFooter {
        is_header: bool,
        apply_to: u8,
    },
    /// One of the core's header/footer templates (0 empty, 1–3 page number
    /// left/centre/right, 4–5 with the file name, 6–10 the same in bold).
    ApplyHeaderFooterTemplate {
        is_header: bool,
        apply_to: u8,
        template: u8,
    },
    /// Bullets on or off, with `bullet` (default ●) as the bullet character.
    ToggleBullet {
        #[serde(default)]
        bullet: Option<String>,
    },
    /// Replace every match in the document; the result reports the count.
    ReplaceAll {
        query: String,
        replacement: String,
        case_sensitive: bool,
    },
    /// A `rows`×`cols` table at a collapsed body caret; the caret moves into
    /// its first cell.
    InsertTable {
        rows: u16,
        cols: u16,
    },
    // The table edits below apply to the top-level table holding the caret.
    InsertRow {
        below: bool,
        #[serde(default = "one")]
        count: u16,
    },
    InsertColumn {
        right: bool,
        #[serde(default = "one")]
        count: u16,
    },
    DeleteRow {},
    DeleteColumn {},
    /// Merges the rectangle spanned by the caret's cell and cell `cell` of
    /// the same table.
    MergeCells {
        cell: u32,
    },
    /// Merges the caret's cell with the cell to its right, or below it.
    MergeRight {},
    MergeDown {},
    /// Splits the caret's cell into `rows`×`cols` cells.
    SplitCell {
        rows: u16,
        cols: u16,
    },
    DeleteTable {},
    /// A web link on the selected text (one paragraph).
    InsertHyperlink {
        uri: String,
    },
    /// The address of the link at the caret.
    UpdateHyperlink {
        uri: String,
    },
    /// Removes the link at the caret; its text stays.
    RemoveHyperlink {},
    /// A bookmark at a body-text caret.
    AddBookmark {
        name: String,
    },
    /// 표 나누기: the caret's row starts a new table below.
    SplitTable {},
    /// 표 붙이기: the next table, after only empty paragraphs, joins this one.
    AttachNextTable {},
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum NativeQuery {
    /// Character properties at the start of the selection.
    CharProperties {},
    /// Paragraph properties of the paragraph holding the selection start.
    ParaProperties {},
    /// Page setup (PageDef) of the section holding the selection start.
    PageDef {},
    /// The link at the selection start, `{"fieldId","start","end","text","uri"}`, or null.
    Hyperlink {},
    /// `{"exists":…,"text":…}` for a header or footer of that section.
    HeaderFooter { is_header: bool, apply_to: u8 },
    /// The first match at or after the selection end (forward), or the last
    /// one before the selection start, wrapping around the document. Does not
    /// move the selection; the host selects the match itself.
    Find {
        query: String,
        forward: bool,
        case_sensitive: bool,
    },
}

impl NativeEditor {
    /// Applies one action as a single undo entry and returns its result JSON.
    pub fn apply_action(
        &mut self,
        expected_revision: u64,
        action: NativeAction,
    ) -> Result<(NativeEditOutcome, Value), NativeHostError> {
        match action.clone() {
            NativeAction::CharFormat { props } => {
                if self.selection.anchor == self.selection.focus {
                    return Err(NativeHostError::InvalidArgument(
                        "character formatting needs a non-empty selection".into(),
                    ));
                }
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let props = resolve_font_name(core, &props);
                    let (start, end) = Self::ordered(selection);
                    let (first, last) = (Self::leaf(start), Self::leaf(end));
                    for paragraph in first..=last {
                        let from = if paragraph == first {
                            start.scalar_offset
                        } else {
                            0
                        };
                        let position = Self::at_leaf(start, paragraph, from);
                        let to = if paragraph == last {
                            end.scalar_offset
                        } else {
                            Self::paragraph_text(core, &position)?.chars().count() as u32
                        };
                        if from >= to {
                            continue;
                        }
                        if position.cell_path.is_empty() {
                            core.apply_char_format_native(
                                start.section as usize,
                                paragraph as usize,
                                from as usize,
                                to as usize,
                                &props,
                            )?;
                        } else {
                            core.apply_char_format_in_cell_by_path(
                                start.section as usize,
                                start.paragraph as usize,
                                &Self::core_path(&position),
                                from as usize,
                                to as usize,
                                &props,
                            )?;
                        }
                    }
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::InsertPageBreak {} | NativeAction::InsertColumnBreak {} => {
                let page = matches!(action, NativeAction::InsertPageBreak {});
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let (start, end) = Self::ordered(selection);
                    if !start.cell_path.is_empty() {
                        return Err(NativeHostError::Unsupported(
                            "page and column breaks go into body text".into(),
                        ));
                    }
                    if start != end {
                        Self::delete_in_container(core, start, end)?;
                    }
                    let (section, paragraph, offset) = (
                        start.section as usize,
                        start.paragraph as usize,
                        start.scalar_offset as usize,
                    );
                    if page {
                        core.insert_page_break_native(section, paragraph, offset)?;
                    } else {
                        core.insert_column_break_native(section, paragraph, offset)?;
                    }
                    let after = Self::at_leaf(start, start.paragraph + 1, 0);
                    Ok(NativeTextSelection {
                        anchor: after.clone(),
                        focus: after,
                    })
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::SetHeaderFooter {
                is_header,
                apply_to,
                text,
            } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    set_header_footer(
                        core,
                        selection.focus.section as usize,
                        is_header,
                        apply_to,
                        &text,
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::DeleteHeaderFooter {
                is_header,
                apply_to,
            } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    core.delete_header_footer_native(
                        selection.focus.section as usize,
                        is_header,
                        apply_to,
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::ApplyHeaderFooterTemplate {
                is_header,
                apply_to,
                template,
            } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    core.apply_hf_template_native(
                        selection.focus.section as usize,
                        is_header,
                        apply_to,
                        template,
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::InsertHyperlink { uri } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let (start, end) = Self::ordered(selection);
                    if start == end || Self::leaf(start) != Self::leaf(end) {
                        return Err(NativeHostError::InvalidArgument(
                            "a link needs selected text within one paragraph".into(),
                        ));
                    }
                    core.insert_hyperlink_native(
                        &link_target(start),
                        start.scalar_offset as usize,
                        end.scalar_offset as usize,
                        &uri,
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::UpdateHyperlink { .. } | NativeAction::RemoveHyperlink {} => {
                let link = self.query(NativeQuery::Hyperlink {})?;
                let Some(field_id) = link["fieldId"].as_u64() else {
                    return Err(NativeHostError::InvalidArgument(
                        "there is no link at the caret".into(),
                    ));
                };
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let target = link_target(&selection.focus);
                    match &action {
                        NativeAction::UpdateHyperlink { uri } => {
                            core.update_hyperlink_native(&target, field_id as u32, uri)?;
                        }
                        _ => core.remove_hyperlink_with_format_native(
                            &target,
                            field_id as u32,
                            true,
                        )?,
                    }
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::AddBookmark { name } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let caret = &selection.focus;
                    if !caret.cell_path.is_empty() {
                        return Err(NativeHostError::Unsupported(
                            "bookmarks go into body text".into(),
                        ));
                    }
                    let result = parse(&core.add_bookmark_native(
                        caret.section as usize,
                        caret.paragraph as usize,
                        caret.scalar_offset as usize,
                        &name,
                    )?)?;
                    if result["ok"] != json!(true) {
                        return Err(NativeHostError::InvalidArgument(
                            result["error"]
                                .as_str()
                                .unwrap_or("bookmark was not added")
                                .into(),
                        ));
                    }
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::SetPageDef { props } => {
                let props = props.to_string();
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    core.set_page_def_native(selection.focus.section as usize, &props)?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::ParaFormat { props } => {
                let props = props.to_string();
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    Self::format_paragraphs(core, selection, &props)?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::ToggleNumbering {} | NativeAction::ToggleBullet { .. } => {
                let (kind, bullet) = match &action {
                    NativeAction::ToggleBullet { bullet } => ("Bullet", bullet.clone()),
                    _ => ("Number", None),
                };
                let turn_off =
                    self.query(NativeQuery::ParaProperties {})?["headType"].as_str() == Some(kind);
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let props = if turn_off {
                        json!({ "headType": "None" })
                    } else {
                        // Created inside the transaction, so undo removes a new
                        // definition along with its use.
                        let id = if kind == "Number" {
                            core.ensure_default_numbering_native()
                        } else {
                            core.ensure_default_bullet_native(bullet.as_deref().unwrap_or("●"))
                        };
                        json!({ "headType": kind, "numberingId": id, "paraLevel": 0 })
                    };
                    Self::format_paragraphs(core, selection, &props.to_string())?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::ReplaceAll {
                query,
                replacement,
                case_sensitive,
            } => {
                let mut result = Value::Null;
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    result =
                        parse(&core.replace_all_native(&query, &replacement, case_sensitive)?)?;
                    // Offsets after the replacements no longer mean the same
                    // text; keep the selection only if it is still valid.
                    Ok(if Self::validate_selection(core, selection).is_ok() {
                        selection.clone()
                    } else {
                        NativeTextSelection {
                            anchor: document_start(),
                            focus: document_start(),
                        }
                    })
                })?;
                Ok((outcome, result))
            }
            NativeAction::InsertTable { rows, cols } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let caret = &selection.focus;
                    if selection.anchor != selection.focus || !caret.cell_path.is_empty() {
                        return Err(NativeHostError::Unsupported(
                            "a table is inserted at a collapsed caret in body text".into(),
                        ));
                    }
                    let created = parse(&core.create_table_native(
                        caret.section as usize,
                        caret.paragraph as usize,
                        caret.scalar_offset as usize,
                        rows,
                        cols,
                    )?)?;
                    let field = |name: &str| {
                        created[name]
                            .as_u64()
                            .map(|value| value as u32)
                            .ok_or_else(|| {
                                NativeHostError::InvalidArgument(
                                    "table creation returned no address".into(),
                                )
                            })
                    };
                    let table = TableAddress {
                        section: caret.section,
                        paragraph: field("paraIdx")?,
                        control: field("controlIdx")?,
                    };
                    Ok(table.caret_in(0))
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::InsertRow { .. }
            | NativeAction::InsertColumn { .. }
            | NativeAction::DeleteRow {}
            | NativeAction::DeleteColumn {}
            | NativeAction::MergeCells { .. }
            | NativeAction::MergeRight {}
            | NativeAction::MergeDown {}
            | NativeAction::SplitCell { .. }
            | NativeAction::DeleteTable {}
            | NativeAction::SplitTable {}
            | NativeAction::AttachNextTable {} => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    table_edit(core, &selection.focus, &action)
                })?;
                Ok((outcome, Value::Null))
            }
        }
    }

    /// Answers a query about the document at the current selection. It does
    /// not change the document; `&mut` is for the core's cell-style lookup.
    pub fn query(&mut self, query: NativeQuery) -> Result<Value, NativeHostError> {
        let (start, end) = Self::ordered(&self.selection);
        let (start, end) = (start.clone(), end.clone());
        let core = &mut self.session.core;
        match query {
            NativeQuery::CharProperties {} if start.cell_path.is_empty() => {
                parse(&core.get_char_properties_at_native(
                    start.section as usize,
                    start.paragraph as usize,
                    start.scalar_offset as usize,
                )?)
            }
            NativeQuery::CharProperties {} => parse(&core.get_cell_char_properties_at_by_path(
                start.section as usize,
                start.paragraph as usize,
                &Self::core_path(&start),
                start.scalar_offset as usize,
            )?),
            NativeQuery::ParaProperties {} if start.cell_path.is_empty() => {
                parse(&core.get_para_properties_at_native(
                    start.section as usize,
                    start.paragraph as usize,
                )?)
            }
            NativeQuery::ParaProperties {} => parse(&core.get_cell_para_properties_at_by_path(
                start.section as usize,
                start.paragraph as usize,
                &Self::core_path(&start),
            )?),
            NativeQuery::PageDef {} => parse(&core.get_page_def_native(start.section as usize)?),
            NativeQuery::Hyperlink {} => {
                let offset = start.scalar_offset as usize;
                let links = core.hyperlinks_native(&link_target(&start))?;
                Ok(links
                    .into_iter()
                    .find(|link| link.start <= offset && offset <= link.end)
                    .map_or(Value::Null, |link| json!(link)))
            }
            NativeQuery::HeaderFooter {
                is_header,
                apply_to,
            } => header_footer_text(core, start.section as usize, is_header, apply_to),
            NativeQuery::Find {
                query,
                forward,
                case_sensitive,
            } => {
                // `search_text_native` only finds matches strictly after its
                // start, which skips a match right at a collapsed caret, and it
                // cannot address nested cells.
                let mut hits = core.native_text_search_hits(&query, case_sensitive);
                hits.sort_by_key(|(position, _)| order_key(position));
                let hit = if forward {
                    let from = order_key(&end);
                    hits.iter()
                        .find(|(position, _)| order_key(position) >= from)
                        .map(|hit| (hit, false))
                        .or_else(|| hits.first().map(|hit| (hit, true)))
                } else {
                    let before = order_key(&start);
                    hits.iter()
                        .rev()
                        .find(|(position, _)| order_key(position) < before)
                        .map(|hit| (hit, false))
                        .or_else(|| hits.last().map(|hit| (hit, true)))
                };
                Ok(match hit {
                    Some(((position, length), wrapped)) => json!({
                        "found": true,
                        "wrapped": wrapped,
                        "position": position,
                        "length": length,
                        "totalMatchCount": hits.len(),
                    }),
                    None => json!({ "found": false }),
                })
            }
        }
    }

    /// Paragraph properties for every paragraph the selection touches, in
    /// the body or in a table cell.
    fn format_paragraphs(
        core: &mut DocumentCore,
        selection: &NativeTextSelection,
        props: &str,
    ) -> Result<(), NativeHostError> {
        let (start, end) = Self::ordered(selection);
        for paragraph in Self::leaf(start)..=Self::leaf(end) {
            if start.cell_path.is_empty() {
                core.apply_para_format_native(start.section as usize, paragraph as usize, props)?;
            } else {
                core.apply_para_format_in_cell_by_path(
                    start.section as usize,
                    start.paragraph as usize,
                    &Self::core_path(&Self::at_leaf(start, paragraph, 0)),
                    props,
                )?;
            }
        }
        Ok(())
    }

    /// Like `edit_transaction`, for actions that change formatting or apply
    /// document-wide: paragraphs that hold controls (tables, fields...) keep
    /// them, so the body-text span check does not apply.
    fn action_transaction(
        &mut self,
        expected_revision: u64,
        edit: impl FnOnce(
            &mut DocumentCore,
            &NativeTextSelection,
        ) -> Result<NativeTextSelection, NativeHostError>,
    ) -> Result<NativeEditOutcome, NativeHostError> {
        self.check_edit_ready(expected_revision)?;
        self.check_mutation_capacity()?;
        self.check_snapshot_slots(2)?;
        let selection_before = self.selection.clone();
        let before = self.session.core.save_snapshot_native();
        let result = edit(&mut self.session.core, &selection_before).and_then(|selection| {
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
        self.record(super::HistoryEntry {
            before,
            after,
            selection_before,
            selection_after,
        });
        Ok(self.outcome(true, true))
    }
}

fn document_start() -> NativeTextPosition {
    NativeTextPosition {
        section: 0,
        paragraph: 0,
        scalar_offset: 0,
        cell_path: Vec::new(),
    }
}

/// `{"exists", "text", "plain"}` for a header/footer. While editing, rhwp
/// keeps its fields as marker characters; a saved and reopened file has them
/// as auto-number controls and a file-path field instead. Both read back as
/// markers here, so the text can be set again with `SetHeaderFooter`.
/// `plain` is false when the header/footer also holds other objects, which
/// replacing its text would remove.
fn header_footer_text(
    core: &DocumentCore,
    section: usize,
    is_header: bool,
    apply_to: u8,
) -> Result<Value, NativeHostError> {
    use crate::model::control::{AutoNumberType, FieldType};
    let found = parse(&core.get_header_footer_native(section, is_header, apply_to)?)?;
    let (Some(paragraph), Some(control)) =
        (found["paraIndex"].as_u64(), found["controlIndex"].as_u64())
    else {
        return Ok(json!({ "exists": false }));
    };
    let paragraphs = match core.document().sections[section].paragraphs[paragraph as usize]
        .controls
        .get(control as usize)
    {
        Some(Control::Header(header)) => &header.paragraphs,
        Some(Control::Footer(footer)) => &footer.paragraphs,
        _ => return Ok(json!({ "exists": false })),
    };
    let mut plain = true;
    let mut lines = Vec::with_capacity(paragraphs.len());
    for paragraph in paragraphs {
        let mut chars: Vec<Option<char>> = paragraph.text.chars().map(Some).collect();
        for (index, (position, control)) in paragraph
            .control_text_positions()
            .into_iter()
            .zip(&paragraph.controls)
            .enumerate()
        {
            match control {
                Control::AutoNumber(number)
                    if matches!(
                        number.number_type,
                        AutoNumberType::Page | AutoNumberType::TotalPage
                    ) && chars.get(position) == Some(&Some(' ')) =>
                {
                    chars[position] = Some(if number.number_type == AutoNumberType::Page {
                        '\u{15}'
                    } else {
                        '\u{16}'
                    });
                }
                Control::Field(field) if field.field_type == FieldType::Path => {
                    if let Some(range) = paragraph
                        .field_ranges
                        .iter()
                        .find(|range| range.control_idx == index)
                    {
                        for slot in chars
                            .iter_mut()
                            .take(range.end_char_idx)
                            .skip(range.start_char_idx)
                        {
                            *slot = None;
                        }
                        if let Some(slot) = chars.get_mut(range.start_char_idx) {
                            *slot = Some('\u{17}');
                        }
                    }
                }
                _ => plain = false,
            }
        }
        lines.push(chars.into_iter().flatten().collect::<String>());
    }
    Ok(json!({ "exists": true, "text": lines.join("\n"), "plain": plain }))
}

/// The page number, page count and file name field markers, by field type.
const FIELD_MARKERS: [(char, u8); 3] = [('\u{15}', 1), ('\u{16}', 2), ('\u{17}', 3)];

/// Replaces all text of a header/footer: first the plain text, then each
/// field marker at its place, left to right.
fn set_header_footer(
    core: &mut DocumentCore,
    section: usize,
    is_header: bool,
    apply_to: u8,
    text: &str,
) -> Result<(), NativeHostError> {
    if parse(&core.get_header_footer_native(section, is_header, apply_to)?)?["exists"]
        != json!(true)
    {
        core.create_header_footer_native(section, is_header, apply_to)?;
    }
    let current = parse(&core.get_header_footer_native(section, is_header, apply_to)?)?;
    let old = current["text"].as_str().unwrap_or("");
    let last = old.split('\n').count() - 1;
    let last_length = old
        .split('\n')
        .next_back()
        .map_or(0, |line| line.chars().count());
    let field = |ch: char| {
        FIELD_MARKERS
            .iter()
            .find(|(marker, _)| *marker == ch)
            .map(|(_, kind)| *kind)
    };
    let plain: String = text.chars().filter(|&ch| field(ch).is_none()).collect();
    core.replace_range_in_header_footer_native(
        section,
        is_header,
        apply_to,
        0,
        0,
        last,
        last_length,
        &plain,
    )?;
    for (paragraph, line) in text.split('\n').enumerate() {
        for (offset, ch) in line.chars().enumerate() {
            if let Some(kind) = field(ch) {
                core.insert_field_in_hf_native(
                    section, is_header, apply_to, paragraph, offset, kind,
                )?;
            }
        }
    }
    Ok(())
}

/// The paragraph (body or table cell) holding `position`, for the core's link calls.
fn link_target(position: &NativeTextPosition) -> crate::document_core::hyperlink::HyperlinkTarget {
    crate::document_core::hyperlink::HyperlinkTarget {
        section: position.section as usize,
        para: position.paragraph as usize,
        cell_path: NativeEditor::core_path(position),
    }
}

fn one() -> u16 {
    1
}

/// A top-level table: the body paragraph holding it and its control index.
struct TableAddress {
    section: u32,
    paragraph: u32,
    control: u32,
}

impl TableAddress {
    fn of(position: &NativeTextPosition) -> Result<(Self, u32), NativeHostError> {
        match position.cell_path.as_slice() {
            [entry] => Ok((
                TableAddress {
                    section: position.section,
                    paragraph: position.paragraph,
                    control: entry.control,
                },
                entry.cell,
            )),
            [] => Err(NativeHostError::InvalidArgument(
                "the caret is not in a table".into(),
            )),
            _ => Err(NativeHostError::Unsupported(
                "table structure edits support top-level tables only".into(),
            )),
        }
    }

    fn table<'a>(&self, core: &'a DocumentCore) -> Result<&'a Table, NativeHostError> {
        match core.document().sections[self.section as usize].paragraphs[self.paragraph as usize]
            .controls
            .get(self.control as usize)
        {
            Some(Control::Table(table)) => Ok(table),
            _ => Err(NativeHostError::InvalidArgument(
                "the caret is not in a table".into(),
            )),
        }
    }

    /// The index of the cell covering grid position (`row`, `col`).
    fn cell_at(&self, core: &DocumentCore, row: u16, col: u16) -> Result<u32, NativeHostError> {
        self.table(core)?
            .cells
            .iter()
            .position(|cell| {
                (cell.row..cell.row.saturating_add(cell.row_span.max(1))).contains(&row)
                    && (cell.col..cell.col.saturating_add(cell.col_span.max(1))).contains(&col)
            })
            .map(|index| index as u32)
            .ok_or_else(|| NativeHostError::InvalidArgument("no cell at that grid position".into()))
    }

    fn caret_in(&self, cell: u32) -> NativeTextSelection {
        let position = NativeTextPosition {
            section: self.section,
            paragraph: self.paragraph,
            scalar_offset: 0,
            cell_path: vec![NativeCellPathEntry {
                control: self.control,
                cell,
                paragraph: 0,
            }],
        };
        NativeTextSelection {
            anchor: position.clone(),
            focus: position,
        }
    }
}

/// One row/column/cell/table edit on the table holding `caret`. Returns the
/// caret afterwards: in the cell now at the caret's grid position (clamped to
/// the table), or before the table's paragraph when the table is gone.
fn table_edit(
    core: &mut DocumentCore,
    caret: &NativeTextPosition,
    action: &NativeAction,
) -> Result<NativeTextSelection, NativeHostError> {
    let (table, cell) = TableAddress::of(caret)?;
    let current =
        table.table(core)?.cells.get(cell as usize).ok_or_else(|| {
            NativeHostError::InvalidArgument("the caret is not in a table".into())
        })?;
    let (row, col) = (current.row, current.col);
    let (section, paragraph, control) = (
        table.section as usize,
        table.paragraph as usize,
        table.control as usize,
    );
    let mut target = (row, col);
    match *action {
        NativeAction::InsertRow { below, count } => {
            for _ in 0..count.clamp(1, 256) {
                core.insert_table_row_native(section, paragraph, control, target.0, below)?;
                if !below {
                    target.0 += 1;
                }
            }
        }
        NativeAction::InsertColumn { right, count } => {
            for _ in 0..count.clamp(1, 256) {
                core.insert_table_column_native(section, paragraph, control, target.1, right)?;
                if !right {
                    target.1 += 1;
                }
            }
        }
        NativeAction::DeleteRow {} | NativeAction::DeleteColumn {} => {
            let shape = table.table(core)?;
            let last = if matches!(action, NativeAction::DeleteRow {}) {
                shape.row_count
            } else {
                shape.col_count
            };
            if last <= 1 {
                // The last row or column: delete the whole table.
                return table_edit(core, caret, &NativeAction::DeleteTable {});
            }
            if matches!(action, NativeAction::DeleteRow {}) {
                core.delete_table_row_native(section, paragraph, control, row)?;
                target.0 = row.min(last - 2);
            } else {
                core.delete_table_column_native(section, paragraph, control, col)?;
                target.1 = col.min(last - 2);
            }
        }
        NativeAction::MergeCells { cell: other } => {
            let other = table
                .table(core)?
                .cells
                .get(other as usize)
                .ok_or_else(|| NativeHostError::InvalidArgument("no such cell".into()))?;
            let (top, left) = (row.min(other.row), col.min(other.col));
            let end = |start: u16, span: u16| start.saturating_add(span.max(1)) - 1;
            let bottom = end(current.row, current.row_span).max(end(other.row, other.row_span));
            let right = end(current.col, current.col_span).max(end(other.col, other.col_span));
            core.merge_table_cells_native(section, paragraph, control, top, left, bottom, right)?;
            target = (top, left);
        }
        NativeAction::MergeRight {} | NativeAction::MergeDown {} => {
            let neighbour = if matches!(action, NativeAction::MergeRight {}) {
                (row, current.col.saturating_add(current.col_span.max(1)))
            } else {
                (current.row.saturating_add(current.row_span.max(1)), col)
            };
            let other = table.cell_at(core, neighbour.0, neighbour.1)?;
            return table_edit(core, caret, &NativeAction::MergeCells { cell: other });
        }
        NativeAction::SplitCell { rows, cols } => {
            if rows == 0 || cols == 0 {
                return Err(NativeHostError::InvalidArgument(
                    "split into at least 1×1".into(),
                ));
            }
            core.split_table_cell_into_native(
                section, paragraph, control, row, col, rows, cols, true, false,
            )?;
        }
        NativeAction::SplitTable {} => {
            core.split_table_native(section, paragraph, control, row)?;
            // The caret's row is the first row of the new table, held by the
            // paragraph after the empty one the split leaves between them.
            let below = TableAddress {
                section: table.section,
                paragraph: table.paragraph + 2,
                control: 0,
            };
            let control = core.document().sections[section]
                .paragraphs
                .get(below.paragraph as usize)
                .and_then(|holder| {
                    holder
                        .controls
                        .iter()
                        .position(|c| matches!(c, Control::Table(_)))
                })
                .ok_or_else(|| {
                    NativeHostError::InvalidArgument("the split left no table below".into())
                })?;
            let below = TableAddress {
                control: control as u32,
                ..below
            };
            return Ok(below.caret_in(below.cell_at(core, 0, col)?));
        }
        NativeAction::AttachNextTable {} => {
            core.merge_table_with_next_native(section, paragraph, control)?;
        }
        NativeAction::DeleteTable {} => {
            core.delete_table_control_native(section, paragraph, control)?;
            let position = NativeTextPosition {
                section: table.section,
                paragraph: table.paragraph,
                scalar_offset: 0,
                cell_path: Vec::new(),
            };
            return Ok(NativeTextSelection {
                anchor: position.clone(),
                focus: position,
            });
        }
        _ => unreachable!("not a table edit"),
    }
    Ok(table.caret_in(table.cell_at(core, target.0, target.1)?))
}

/// A total order over positions for find next/previous: section, body
/// paragraph, then the cell path, then the offset. A table's cells sort after
/// the body text of the paragraph that holds the table.
fn order_key(position: &NativeTextPosition) -> Vec<u64> {
    let mut key = vec![position.section as u64, position.paragraph as u64];
    for entry in &position.cell_path {
        key.extend([
            1,
            entry.control as u64,
            entry.cell as u64,
            entry.paragraph as u64,
        ]);
    }
    key.extend([0, position.scalar_offset as u64]);
    key
}

/// Replaces a `fontName` property with the document's `fontId` for it.
fn resolve_font_name(core: &mut DocumentCore, props: &Value) -> String {
    let mut props = props.clone();
    if let Some(name) = props
        .get("fontName")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        let id = core.find_or_create_font_id_native(&name);
        if let Some(object) = props.as_object_mut() {
            object.remove("fontName");
            if id >= 0 {
                object.insert("fontId".into(), json!(id));
            }
        }
    }
    props.to_string()
}

fn parse(text: &str) -> Result<Value, NativeHostError> {
    serde_json::from_str(text).map_err(|error| {
        NativeHostError::InvalidArgument(format!("engine returned invalid JSON: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_host::NativeDocumentSession;

    fn position(offset: u32) -> NativeTextPosition {
        NativeTextPosition {
            section: 0,
            paragraph: 0,
            scalar_offset: offset,
            cell_path: Vec::new(),
        }
    }

    fn selected(anchor: u32, focus: u32) -> NativeTextSelection {
        NativeTextSelection {
            anchor: position(anchor),
            focus: position(focus),
        }
    }

    fn editor(text: &str, selection: NativeTextSelection) -> NativeEditor {
        let mut core = DocumentCore::new_empty();
        core.create_blank_document_native().unwrap();
        core.insert_text_native(0, 0, 0, text).unwrap();
        NativeEditor::new(NativeDocumentSession::from_core(core), selection).unwrap()
    }

    fn current_revision(editor: &NativeEditor) -> u64 {
        editor.session().revision()
    }

    fn text(editor: &NativeEditor) -> String {
        editor.session().document().document().sections[0].paragraphs[0]
            .text
            .clone()
    }

    fn char_property(editor: &mut NativeEditor, offset: u32, key: &str) -> Value {
        let revision = current_revision(editor);
        editor
            .set_selection(revision, selected(offset, offset + 1))
            .unwrap();
        editor.query(NativeQuery::CharProperties {}).unwrap()[key].clone()
    }

    fn action(json: Value) -> NativeAction {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn char_format_changes_only_the_selection_and_undoes_as_one_entry() {
        let mut editor = editor("가나다라", selected(1, 3));
        let revision = current_revision(&editor);
        let (outcome, _) = editor
            .apply_action(
                revision,
                action(json!({"action": "charFormat", "props": {"bold": true}})),
            )
            .unwrap();
        assert_eq!(outcome.selection, selected(1, 3));
        assert_eq!(char_property(&mut editor, 0, "bold"), json!(false));
        assert_eq!(char_property(&mut editor, 1, "bold"), json!(true));
        assert_eq!(char_property(&mut editor, 2, "bold"), json!(true));
        assert_eq!(char_property(&mut editor, 3, "bold"), json!(false));
        let revision = current_revision(&editor);
        editor.undo(revision).unwrap();
        assert_eq!(char_property(&mut editor, 1, "bold"), json!(false));
        assert_eq!(text(&editor), "가나다라");
    }

    #[test]
    fn char_format_rejects_collapsed_selection_without_a_history_entry() {
        let mut editor = editor("가나", selected(1, 1));
        let revision = current_revision(&editor);
        let error = editor
            .apply_action(
                revision,
                action(json!({"action": "charFormat", "props": {"bold": true}})),
            )
            .unwrap_err();
        assert!(matches!(error, NativeHostError::InvalidArgument(_)));
        assert_eq!(editor.session().revision(), revision);
    }

    #[test]
    fn para_format_sets_alignment() {
        let mut editor = editor("문단", selected(0, 0));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "paraFormat", "props": {"alignment": "center"}})),
            )
            .unwrap();
        let properties = editor.query(NativeQuery::ParaProperties {}).unwrap();
        assert_eq!(properties["alignment"], json!("center"));
    }

    fn find(editor: &mut NativeEditor, query: &str, forward: bool) -> Value {
        editor
            .query(NativeQuery::Find {
                query: query.into(),
                forward,
                case_sensitive: true,
            })
            .unwrap()
    }

    #[test]
    fn find_matches_at_a_collapsed_caret_moves_past_a_selected_match_and_wraps() {
        let mut editor = editor("ab ab", selected(0, 0));
        assert_eq!(
            find(&mut editor, "ab", true)["position"]["scalarOffset"],
            json!(0)
        );
        let revision = current_revision(&editor);
        editor.set_selection(revision, selected(0, 2)).unwrap();
        let next = find(&mut editor, "ab", true);
        assert_eq!(
            (
                next["position"]["scalarOffset"].clone(),
                next["wrapped"].clone()
            ),
            (json!(3), json!(false))
        );
        let revision = current_revision(&editor);
        editor.set_selection(revision, selected(3, 5)).unwrap();
        let wrapped = find(&mut editor, "ab", true);
        assert_eq!(
            (
                wrapped["position"]["scalarOffset"].clone(),
                wrapped["wrapped"].clone()
            ),
            (json!(0), json!(true))
        );
        assert_eq!(
            find(&mut editor, "ab", false)["position"]["scalarOffset"],
            json!(0)
        );
        assert_eq!(wrapped["totalMatchCount"], json!(2));
        assert_eq!(find(&mut editor, "zz", true), json!({"found": false}));
    }

    #[test]
    fn replace_all_reports_count_and_undoes_as_one_entry() {
        let mut editor = editor("a A a", selected(0, 0));
        let revision = current_revision(&editor);
        let (_, result) = editor
            .apply_action(
                revision,
                action(json!({"action": "replaceAll", "query": "a", "replacement": "bb", "caseSensitive": true})),
            )
            .unwrap();
        assert_eq!(result["count"], json!(2));
        assert_eq!(text(&editor), "bb A bb");
        let revision = current_revision(&editor);
        editor.undo(revision).unwrap();
        assert_eq!(text(&editor), "a A a");
    }

    #[test]
    fn numbering_and_bullets_toggle_and_undo_removes_a_new_definition() {
        let mut editor = editor("항목", selected(0, 0));
        let bullets_before = editor
            .session()
            .document()
            .document()
            .doc_info
            .bullets
            .len();
        let revision = current_revision(&editor);
        editor
            .apply_action(revision, action(json!({"action": "toggleBullet"})))
            .unwrap();
        let properties = editor.query(NativeQuery::ParaProperties {}).unwrap();
        assert_eq!(properties["headType"], json!("Bullet"));
        let revision = current_revision(&editor);
        editor
            .apply_action(revision, action(json!({"action": "toggleNumbering"})))
            .unwrap();
        assert_eq!(
            editor.query(NativeQuery::ParaProperties {}).unwrap()["headType"],
            json!("Number")
        );
        let revision = current_revision(&editor);
        editor
            .apply_action(revision, action(json!({"action": "toggleNumbering"})))
            .unwrap();
        assert_eq!(
            editor.query(NativeQuery::ParaProperties {}).unwrap()["headType"],
            json!("None")
        );
        for _ in 0..3 {
            let revision = current_revision(&editor);
            editor.undo(revision).unwrap();
        }
        assert_eq!(
            editor
                .session()
                .document()
                .document()
                .doc_info
                .bullets
                .len(),
            bullets_before
        );
    }

    #[test]
    fn page_break_moves_the_caret_to_a_new_paragraph_on_the_next_page() {
        let mut editor = editor("앞뒤", selected(1, 1));
        let pages = editor.session().document().page_count();
        let revision = current_revision(&editor);
        let (outcome, _) = editor
            .apply_action(revision, action(json!({"action": "insertPageBreak"})))
            .unwrap();
        let mut after = position(0);
        after.paragraph = 1;
        assert_eq!(outcome.selection.focus, after);
        let paragraphs = &editor.session().document().document().sections[0].paragraphs;
        assert_eq!(
            (paragraphs[0].text.as_str(), paragraphs[1].text.as_str()),
            ("앞", "뒤")
        );
        assert_eq!(editor.session().document().page_count(), pages + 1);
        let revision = current_revision(&editor);
        editor.undo(revision).unwrap();
        assert_eq!(text(&editor), "앞뒤");
    }

    #[test]
    fn page_setup_round_trips_through_the_query() {
        let mut editor = editor("용지", selected(0, 0));
        let mut page = editor.query(NativeQuery::PageDef {}).unwrap();
        page["landscape"] = json!(true);
        page["marginLeft"] = json!(5669);
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "setPageDef", "props": page})),
            )
            .unwrap();
        let updated = editor.query(NativeQuery::PageDef {}).unwrap();
        assert_eq!(updated["landscape"], json!(true));
        assert_eq!(updated["marginLeft"], json!(5669));
    }

    fn header_text(editor: &mut NativeEditor) -> Value {
        editor
            .query(NativeQuery::HeaderFooter {
                is_header: true,
                apply_to: 0,
            })
            .unwrap()
    }

    #[test]
    fn header_text_with_fields_is_set_replaced_and_deleted() {
        let mut editor = editor("본문", selected(0, 0));
        assert_eq!(header_text(&mut editor)["exists"], json!(false));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(
                    json!({"action": "setHeaderFooter", "isHeader": true, "applyTo": 0,
                              "text": "보고서\n- \u{15} / \u{16} -"}),
                ),
            )
            .unwrap();
        let header = header_text(&mut editor);
        assert_eq!(header["exists"], json!(true));
        assert_eq!(header["text"], json!("보고서\n- \u{15} / \u{16} -"));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "setHeaderFooter", "isHeader": true, "applyTo": 0, "text": "새 머리말"})),
            )
            .unwrap();
        assert_eq!(header_text(&mut editor)["text"], json!("새 머리말"));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "deleteHeaderFooter", "isHeader": true, "applyTo": 0})),
            )
            .unwrap();
        assert_eq!(header_text(&mut editor)["exists"], json!(false));
        let revision = current_revision(&editor);
        editor.undo(revision).unwrap();
        assert_eq!(header_text(&mut editor)["text"], json!("새 머리말"));
        assert_eq!(text(&editor), "본문");
    }

    #[test]
    fn page_number_template_creates_a_footer_with_the_field() {
        let mut editor = editor("본문", selected(0, 0));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "applyHeaderFooterTemplate", "isHeader": false, "applyTo": 0, "template": 2})),
            )
            .unwrap();
        let footer = editor
            .query(NativeQuery::HeaderFooter {
                is_header: false,
                apply_to: 0,
            })
            .unwrap();
        assert_eq!(footer["exists"], json!(true));
        assert!(footer["text"].as_str().unwrap().contains('\u{15}'));
    }

    #[test]
    fn header_fields_read_back_as_markers_after_a_save() {
        let mut editor = editor("본문", selected(0, 0));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(
                    json!({"action": "setHeaderFooter", "isHeader": true, "applyTo": 0,
                              "text": "쪽 \u{15} / \u{16}"}),
                ),
            )
            .unwrap();
        // The save path a host uses; it turns field markers into controls.
        let prepared = editor
            .prepare_save(crate::native_host::save::NativeSaveFormat::Hwp)
            .unwrap();
        let reopened = DocumentCore::from_bytes(prepared.bytes()).unwrap();
        let mut reopened =
            NativeEditor::new(NativeDocumentSession::from_core(reopened), selected(0, 0)).unwrap();
        let header = header_text(&mut reopened);
        assert_eq!(header["text"], json!("쪽 \u{15} / \u{16}"));
        assert_eq!(header["plain"], json!(true));
    }

    #[test]
    fn hyperlink_insert_update_remove_and_bookmark() {
        let mut editor = editor("홈페이지 주소", selected(0, 4));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "insertHyperlink", "uri": "https://example.com"})),
            )
            .unwrap();
        let revision = current_revision(&editor);
        editor.set_selection(revision, selected(2, 2)).unwrap();
        let link = editor.query(NativeQuery::Hyperlink {}).unwrap();
        assert_eq!(link["text"], json!("홈페이지"));
        assert_eq!(link["uri"], json!("https://example.com"));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "updateHyperlink", "uri": "https://example.org"})),
            )
            .unwrap();
        assert_eq!(
            editor.query(NativeQuery::Hyperlink {}).unwrap()["uri"],
            json!("https://example.org")
        );
        let revision = current_revision(&editor);
        editor
            .apply_action(revision, action(json!({"action": "removeHyperlink"})))
            .unwrap();
        assert_eq!(
            editor.query(NativeQuery::Hyperlink {}).unwrap(),
            Value::Null
        );
        assert_eq!(text(&editor), "홈페이지 주소");
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "addBookmark", "name": "처음"})),
            )
            .unwrap();
        let revision = current_revision(&editor);
        let duplicate = editor
            .apply_action(
                revision,
                action(json!({"action": "addBookmark", "name": "처음"})),
            )
            .unwrap_err();
        assert!(matches!(duplicate, NativeHostError::InvalidArgument(_)));
    }
}
