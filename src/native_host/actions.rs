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
    /// 다단: columns of the caret's section. `kind` 0 normal, 1 distribute,
    /// 2 parallel; `spacing` between columns in HWPUNIT.
    SetColumns {
        count: u16,
        #[serde(default)]
        kind: u8,
        #[serde(default = "same_width")]
        same_width: bool,
        spacing: i16,
    },
    /// 구역 설정 of the caret's section: `pageNum` (start), `hideHeader`,
    /// `hideFooter`, `hideMasterPage`, `hideBorder`, `hideFill` (first page),
    /// `defaultTabSpacing`, `columnSpacing`, … (set_section_def_native keys).
    SetSectionDef {
        props: Value,
    },
    /// 쪽 테두리/배경 of the caret's section: `borderLeft`…`borderBottom`
    /// `{type,width,color}`, `fillType`, `fillColor`, `basis`, `spacing*`, …
    SetPageBorder {
        props: Value,
    },
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
    /// A picture floating at a body-text caret, as 한글 inserts one: anchored
    /// to the caret's paragraph, placed where the caret is on the page.
    /// `data` is the image file in base64; sizes are HWPUNIT and pixels.
    InsertPicture {
        data: String,
        extension: String,
        width: u32,
        height: u32,
        natural_width: u32,
        natural_height: u32,
    },
    /// A shape floating in front of the text where a body-text caret is:
    /// `kind` rectangle, ellipse, line or textbox (an empty text box).
    InsertShape {
        kind: String,
        width: u32,
        height: u32,
    },
    /// Deletes the picture, shape or table control `control` of body
    /// paragraph `paragraph` (as listed by `pageObjects`).
    DeleteObject {
        section: u32,
        paragraph: u32,
        control: u32,
    },
    /// Picture properties (`width`, `height`, `rotationAngle`, `horzFlip`,
    /// `vertFlip`, …) of a picture control.
    SetPictureProperties {
        section: u32,
        paragraph: u32,
        control: u32,
        props: Value,
    },
    /// Properties of a picture, shape or table control, in the engine's
    /// own names (`width`, `rotationAngle`, `borderColor`, `fillBgColor`,
    /// `hasCaption`, …); `hasCaption` adds a numbered 그림/표 caption.
    SetObjectProperties {
        section: u32,
        paragraph: u32,
        control: u32,
        props: Value,
    },
    /// 개체 묶기: two or more pictures or shapes of one section (as listed
    /// by `pageObjects`) become one group; the result is the group's address.
    GroupObjects {
        section: u32,
        objects: Vec<ObjectRef>,
    },
    /// 개체 풀기: a group back into its pictures and shapes, one level.
    UngroupObject {
        section: u32,
        paragraph: u32,
        control: u32,
    },
    /// Stacking order of a picture or shape: front, back, forward, backward.
    ObjectOrder {
        section: u32,
        paragraph: u32,
        control: u32,
        operation: String,
    },
    /// The document style `id` on every paragraph the selection touches
    /// (body text or top-level table cells).
    ApplyStyle {
        id: u32,
    },
    /// Properties of the caret's cell in a top-level table (`fillType`,
    /// `fillColor`, `verticalAlign`, `padding*`, `border*`, …).
    SetCellProperties {
        props: Value,
    },
    /// 계산식 in the caret's cell of a top-level table (`=SUM(ABOVE)`,
    /// `=A1+B1`, …); `format` is `%g` or `%.Nf`, with a trailing `,` for
    /// thousands separators. The cell shows the result.
    SetCellFormula {
        formula: String,
        format: String,
    },
    /// The number in the first paragraph of the caret's cell: `thousands`
    /// adds (or removes) thousands separators, `addDecimal` appends a 0,
    /// `removeDecimal` drops the last decimal digit, as the web editor does.
    FormatCellNumber {
        operation: String,
    },
    /// 감추기 on the caret's body paragraph, from its page on.
    SetPageHide {
        header: bool,
        footer: bool,
        master_page: bool,
        border: bool,
        fill: bool,
        page_number: bool,
    },
    /// 새 쪽 번호: page numbering restarts at `start` from the caret's page.
    NewPageNumber {
        start: u16,
    },
    /// Removes the field (누름틀 and the like) around a body-text caret.
    RemoveField {},
    /// 표 나누기: the caret's row starts a new table below.
    SplitTable {},
    /// 표 붙이기: the next table, after only empty paragraphs, joins this one.
    AttachNextTable {},
}

/// A control of a body paragraph in a known section.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectRef {
    pub paragraph: u32,
    pub control: u32,
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
    /// The objects laid out on page `page` with their page boxes (layout
    /// pixels) and addresses: the core's page control layout.
    PageObjects { page: u32 },
    /// Properties of the caret's cell.
    CellProperties {},
    /// 감추기 flags of the caret's body paragraph.
    PageHide {},
    /// The field around a body-text caret, `{"inField":false}` if none.
    FieldInfo {},
    /// Properties of a picture, shape or table control, with `kind`
    /// `picture`, `shape` or `table`.
    ObjectProperties {
        section: u32,
        paragraph: u32,
        control: u32,
    },
    /// Properties of a picture control.
    PictureProperties {
        section: u32,
        paragraph: u32,
        control: u32,
    },
    /// 구역 설정 of the section holding the selection start.
    SectionDef {},
    /// 쪽 테두리/배경 of that section.
    PageBorder {},
    /// `{"count","kind","sameWidth","spacing"}`: that section's columns.
    Columns {},
    /// `{"sections":[html…]}`: each section's body as the clipboard HTML
    /// fragment, which the web editor's HTML and Word (.doc) export wraps.
    DocumentHtml {},
    /// The link at the selection start, `{"fieldId","start","end","text","uri"}`, or null.
    Hyperlink {},
    /// `{"styles":[{"id","name","englishName","type"}],"current":id}`: the
    /// document's styles and the style of the paragraph at the selection start.
    Styles {},
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
            NativeAction::InsertPicture {
                data,
                extension,
                width,
                height,
                natural_width,
                natural_height,
            } => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data.as_bytes())
                    .map_err(|_| {
                        NativeHostError::InvalidArgument("picture data is not base64".into())
                    })?;
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let caret = &selection.focus;
                    if !caret.cell_path.is_empty() {
                        return Err(NativeHostError::Unsupported(
                            "pictures are inserted in body text".into(),
                        ));
                    }
                    // The caret's place on its page, in HWPUNIT (75 per layout pixel).
                    let rect = core.native_caret_rect(caret)?;
                    let to_units = |pixels: f64| (pixels * 75.0).round() as i32;
                    core.insert_picture_native(
                        caret.section as usize,
                        caret.paragraph as usize,
                        caret.scalar_offset as usize,
                        &[],
                        &bytes,
                        width,
                        height,
                        natural_width,
                        natural_height,
                        &extension,
                        "",
                        Some(to_units(rect.x)),
                        Some(to_units(rect.y)),
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::InsertShape {
                kind,
                width,
                height,
            } => {
                if !matches!(kind.as_str(), "rectangle" | "ellipse" | "line" | "textbox") {
                    return Err(NativeHostError::InvalidArgument(
                        "shape: rectangle, ellipse, line or textbox".into(),
                    ));
                }
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let caret = body_caret(&selection.focus, "shapes are inserted in body text")?;
                    let rect = core.native_caret_rect(caret)?;
                    let to_units = |pixels: f64| (pixels.max(0.0) * 75.0).round() as u32;
                    core.create_shape_control_native(
                        caret.section as usize,
                        caret.paragraph as usize,
                        caret.scalar_offset as usize,
                        width,
                        height,
                        to_units(rect.x),
                        to_units(rect.y),
                        false,
                        "InFrontOfText",
                        &kind,
                        false,
                        false,
                        &[],
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::DeleteObject {
                section,
                paragraph,
                control,
            } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let (s, p, c) = (section as usize, paragraph as usize, control as usize);
                    match object_kind(core, s, p, c) {
                        Some("picture") => core.delete_picture_control_native(s, p, c)?,
                        Some("shape") => core.delete_shape_control_native(s, p, c)?,
                        Some(_) => core.delete_table_control_native(s, p, c)?,
                        None => {
                            return Err(NativeHostError::Unsupported(
                                "only pictures, shapes and tables can be deleted as objects".into(),
                            ))
                        }
                    };
                    // The caret's offsets stay valid; fall back to the paragraph start.
                    Ok(if Self::validate_selection(core, selection).is_ok() {
                        selection.clone()
                    } else {
                        let start = NativeTextPosition {
                            section,
                            paragraph,
                            scalar_offset: 0,
                            cell_path: Vec::new(),
                        };
                        NativeTextSelection {
                            anchor: start.clone(),
                            focus: start,
                        }
                    })
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::SetPictureProperties {
                section,
                paragraph,
                control,
                props,
            } => {
                let props = props.to_string();
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    core.set_picture_properties_native(
                        section as usize,
                        paragraph as usize,
                        control as usize,
                        &props,
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::SetObjectProperties {
                section,
                paragraph,
                control,
                props,
            } => {
                let props = props.to_string();
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let (s, p, c) = (section as usize, paragraph as usize, control as usize);
                    match object_kind(core, s, p, c) {
                        Some("picture") => core.set_picture_properties_native(s, p, c, &props)?,
                        Some("shape") => core.set_shape_properties_native(s, p, c, &props)?,
                        Some(_) => core.set_table_properties_native(s, p, c, &props)?,
                        None => {
                            return Err(NativeHostError::Unsupported(
                                "object properties apply to pictures, shapes and tables".into(),
                            ))
                        }
                    };
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::GroupObjects { section, objects } => {
                let mut result = Value::Null;
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let targets: Vec<(usize, usize)> = objects
                        .iter()
                        .map(|object| (object.paragraph as usize, object.control as usize))
                        .collect();
                    if targets.iter().any(|&(p, c)| {
                        !matches!(
                            object_kind(core, section as usize, p, c),
                            Some("picture" | "shape")
                        )
                    }) {
                        return Err(NativeHostError::Unsupported(
                            "only pictures and shapes can be grouped".into(),
                        ));
                    }
                    let created = parse(&core.group_shapes_native(section as usize, &targets)?)?;
                    result = json!({
                        "section": section,
                        "paragraph": created["paraIdx"],
                        "control": created["controlIdx"],
                    });
                    Ok(Self::validate_selection(core, selection)
                        .map(|_| selection.clone())
                        .unwrap_or_else(|_| paragraph_start(section, targets[0].0 as u32)))
                })?;
                Ok((outcome, result))
            }
            NativeAction::UngroupObject {
                section,
                paragraph,
                control,
            } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    core.ungroup_shape_native(
                        section as usize,
                        paragraph as usize,
                        control as usize,
                    )?;
                    Ok(Self::validate_selection(core, selection)
                        .map(|_| selection.clone())
                        .unwrap_or_else(|_| paragraph_start(section, paragraph)))
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::ObjectOrder {
                section,
                paragraph,
                control,
                operation,
            } => {
                if !matches!(
                    operation.as_str(),
                    "front" | "back" | "forward" | "backward"
                ) {
                    return Err(NativeHostError::InvalidArgument(
                        "order: front, back, forward, backward".into(),
                    ));
                }
                let is_shape = matches!(
                    self.session
                        .core
                        .document()
                        .sections
                        .get(section as usize)
                        .and_then(|s| s.paragraphs.get(paragraph as usize))
                        .and_then(|p| p.controls.get(control as usize)),
                    Some(Control::Shape(_))
                );
                if !is_shape {
                    // The web editor orders shapes only, too.
                    return Err(NativeHostError::Unsupported(
                        "stacking order applies to shapes".into(),
                    ));
                }
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    core.change_shape_z_order_native(
                        section as usize,
                        paragraph as usize,
                        control as usize,
                        &operation,
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::SetCellProperties { props } => {
                let props = props.to_string();
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let (table, cell) = TableAddress::of(&selection.focus)?;
                    core.set_cell_properties_native(
                        table.section as usize,
                        table.paragraph as usize,
                        table.control as usize,
                        cell as usize,
                        &props,
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::SetCellFormula { formula, format } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let (table, cell) = TableAddress::of(&selection.focus)?;
                    core.set_table_cell_formula(
                        table.section as usize,
                        table.paragraph as usize,
                        table.control as usize,
                        cell as usize,
                        &formula,
                        &format,
                    )?;
                    Ok(Self::validate_selection(core, selection)
                        .map(|_| selection.clone())
                        .unwrap_or_else(|_| table.caret_in(cell)))
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::FormatCellNumber { operation } => {
                let focus = self.selection.focus.clone();
                let (_, cell) = TableAddress::of(&focus)?;
                let mut first = focus.clone();
                if let Some(entry) = first.cell_path.last_mut() {
                    entry.cell = cell;
                    entry.paragraph = 0;
                }
                first.scalar_offset = 0;
                let text = Self::paragraph_text(&self.session.core, &first)?.to_owned();
                let Some(formatted) = format_cell_number(&text, &operation) else {
                    return Err(NativeHostError::InvalidArgument(
                        "the cell does not hold a number".into(),
                    ));
                };
                let mut end = first.clone();
                end.scalar_offset = text.chars().count() as u32;
                let outcome = self.action_transaction(expected_revision, |core, _| {
                    Self::delete_in_container(core, &first, &end)?;
                    Self::insert_in_container(core, &first, &formatted)?;
                    Self::recalculate_formulas(core, &first)?;
                    let mut caret = first.clone();
                    caret.scalar_offset = formatted.chars().count() as u32;
                    Ok(NativeTextSelection {
                        anchor: caret.clone(),
                        focus: caret,
                    })
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::SetPageHide {
                header,
                footer,
                master_page,
                border,
                fill,
                page_number,
            } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let caret = body_caret(&selection.focus, "감추기 goes on a body paragraph")?;
                    core.set_page_hide_native(
                        caret.section as usize,
                        caret.paragraph as usize,
                        header,
                        footer,
                        master_page,
                        border,
                        fill,
                        page_number,
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::NewPageNumber { start } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let caret = body_caret(&selection.focus, "새 쪽 번호 goes in body text")?;
                    core.insert_new_number_native(
                        caret.section as usize,
                        caret.paragraph as usize,
                        caret.scalar_offset as usize,
                        start.max(1),
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::RemoveField {} => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let caret = body_caret(&selection.focus, "fields are removed in body text")?;
                    core.remove_field_at(
                        caret.section as usize,
                        caret.paragraph as usize,
                        caret.scalar_offset as usize,
                    )?;
                    // The field's text went with it; keep the caret if it still fits.
                    Ok(if Self::validate_selection(core, selection).is_ok() {
                        selection.clone()
                    } else {
                        let start = NativeTextPosition {
                            scalar_offset: 0,
                            ..caret.clone()
                        };
                        NativeTextSelection {
                            anchor: start.clone(),
                            focus: start,
                        }
                    })
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::ApplyStyle { id } => {
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let (start, end) = Self::ordered(selection);
                    for paragraph in Self::leaf(start)..=Self::leaf(end) {
                        match start.cell_path.as_slice() {
                            [] => core.apply_style_native(
                                start.section as usize,
                                paragraph as usize,
                                id as usize,
                            )?,
                            [cell] => core.apply_cell_style_native(
                                start.section as usize,
                                start.paragraph as usize,
                                cell.control as usize,
                                cell.cell as usize,
                                paragraph as usize,
                                id as usize,
                            )?,
                            _ => {
                                return Err(NativeHostError::Unsupported(
                                    "styles apply to body text and top-level table cells".into(),
                                ))
                            }
                        };
                    }
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::SetColumns {
                count,
                kind,
                same_width,
                spacing,
            } => {
                if !(1..=255).contains(&count) || kind > 2 {
                    return Err(NativeHostError::InvalidArgument(
                        "columns: 1–255, kind 0–2".into(),
                    ));
                }
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    core.set_column_def_native(
                        selection.focus.section as usize,
                        count,
                        kind,
                        same_width,
                        spacing,
                    )?;
                    Ok(selection.clone())
                })?;
                Ok((outcome, Value::Null))
            }
            NativeAction::SetSectionDef { props } | NativeAction::SetPageBorder { props } => {
                let border = matches!(action, NativeAction::SetPageBorder { .. });
                let props = props.to_string();
                let outcome = self.action_transaction(expected_revision, |core, selection| {
                    let section = selection.focus.section as usize;
                    if border {
                        core.set_page_border_fill_native(section, &props)?;
                    } else {
                        core.set_section_def_native(section, &props)?;
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
            NativeQuery::DocumentHtml {} => {
                let mut sections = Vec::new();
                for (index, section) in core.document().sections.iter().enumerate() {
                    let Some(last) = section.paragraphs.len().checked_sub(1) else {
                        continue;
                    };
                    let end = section.paragraphs[last].text.chars().count();
                    sections.push(core.export_selection_html_native(index, 0, 0, last, end)?);
                }
                Ok(json!({ "sections": sections }))
            }
            NativeQuery::PageObjects { page } => parse(&core.get_page_control_layout_native(page)?),
            NativeQuery::CellProperties {} => {
                let (_, cell) = TableAddress::of(&start)?;
                parse(&core.get_cell_properties_by_cell_path_native(
                    start.section as usize,
                    start.paragraph as usize,
                    &Self::core_path(&start),
                    cell as usize,
                )?)
            }
            NativeQuery::PageHide {} => {
                let caret = body_caret(&start, "감추기 is on body paragraphs")?;
                parse(&core.get_page_hide_native(caret.section as usize, caret.paragraph as usize)?)
            }
            NativeQuery::FieldInfo {} => {
                if !start.cell_path.is_empty() {
                    return Ok(json!({ "inField": false }));
                }
                parse(&core.get_field_info_at(
                    start.section as usize,
                    start.paragraph as usize,
                    start.scalar_offset as usize,
                ))
            }
            NativeQuery::ObjectProperties {
                section,
                paragraph,
                control,
            } => {
                let (s, p, c) = (section as usize, paragraph as usize, control as usize);
                let kind = object_kind(core, s, p, c).ok_or_else(|| {
                    NativeHostError::Unsupported(
                        "object properties apply to pictures, shapes and tables".into(),
                    )
                })?;
                let mut props = parse(&match kind {
                    "picture" => core.get_picture_properties_native(s, p, c)?,
                    "shape" => core.get_shape_properties_native(s, p, c)?,
                    _ => core.get_table_properties_native(s, p, c)?,
                })?;
                props["kind"] = json!(kind);
                Ok(props)
            }
            NativeQuery::PictureProperties {
                section,
                paragraph,
                control,
            } => parse(&core.get_picture_properties_native(
                section as usize,
                paragraph as usize,
                control as usize,
            )?),
            NativeQuery::SectionDef {} => {
                parse(&core.get_section_def_native(start.section as usize)?)
            }
            NativeQuery::PageBorder {} => {
                parse(&core.get_page_border_fill_native(start.section as usize)?)
            }
            NativeQuery::Columns {} => {
                use crate::model::page::ColumnType;
                let columns = DocumentCore::find_initial_column_def(
                    &core.document().sections[start.section as usize].paragraphs,
                );
                let kind = match columns.column_type {
                    ColumnType::Normal => 0,
                    ColumnType::Distribute => 1,
                    ColumnType::Parallel => 2,
                };
                Ok(json!({
                    "count": columns.column_count.max(1),
                    "kind": kind,
                    "sameWidth": columns.same_width,
                    "spacing": columns.spacing,
                }))
            }
            NativeQuery::Styles {} => {
                let styles: Vec<Value> = core
                    .document()
                    .doc_info
                    .styles
                    .iter()
                    .enumerate()
                    .map(|(id, style)| {
                        json!({
                            "id": id,
                            "name": style.local_name,
                            "englishName": style.english_name,
                            "type": style.style_type,
                        })
                    })
                    .collect();
                let current = Self::container(core, &start)?
                    .get(Self::leaf(&start) as usize)
                    .map(|paragraph| paragraph.style_id);
                Ok(json!({ "styles": styles, "current": current }))
            }
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

fn same_width() -> bool {
    true
}

/// The web editor's 세 자리 쉼표 / 자릿수 늘리기 / 자릿수 줄이기 on a cell's text;
/// None when the text is not a plain (optionally signed, comma-grouped) number.
fn format_cell_number(text: &str, operation: &str) -> Option<String> {
    let trimmed = text.trim();
    let (sign, rest) = match trimmed.chars().next()? {
        '+' | '-' => trimmed.split_at(1),
        _ => ("", trimmed),
    };
    let had_commas = rest.contains(',');
    let digits = rest.replace(',', "");
    let (integer, decimals) = match digits.split_once('.') {
        Some((integer, decimals)) => (integer.to_owned(), Some(decimals.to_owned())),
        None => (digits.clone(), None),
    };
    if integer.is_empty()
        || !integer.chars().all(|c| c.is_ascii_digit())
        || !decimals
            .as_deref()
            .unwrap_or("")
            .chars()
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let group = |integer: &str| {
        let mut grouped = String::new();
        for (index, digit) in integer.chars().enumerate() {
            if index > 0 && (integer.len() - index) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(digit);
        }
        grouped
    };
    let integer_text = |commas: bool| {
        if commas {
            group(&integer)
        } else {
            integer.clone()
        }
    };
    let joined = |integer: String, decimals: Option<&str>| match decimals {
        Some(decimals) if !decimals.is_empty() => format!("{sign}{integer}.{decimals}"),
        _ => format!("{sign}{integer}"),
    };
    match operation {
        "thousands" => Some(joined(integer_text(!had_commas), decimals.as_deref())),
        "addDecimal" => Some(format!(
            "{sign}{}.{}0",
            integer_text(had_commas),
            decimals.as_deref().unwrap_or("")
        )),
        "removeDecimal" => {
            let decimals = decimals.filter(|d| !d.is_empty())?;
            Some(joined(
                integer_text(had_commas),
                Some(&decimals[..decimals.len() - 1]),
            ))
        }
        _ => None,
    }
}

/// `position` if it is in body text, else an "unsupported" error.
fn body_caret<'a>(
    position: &'a NativeTextPosition,
    message: &str,
) -> Result<&'a NativeTextPosition, NativeHostError> {
    if position.cell_path.is_empty() {
        Ok(position)
    } else {
        Err(NativeHostError::Unsupported(message.into()))
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

/// A collapsed caret at the start of a body paragraph.
fn paragraph_start(section: u32, paragraph: u32) -> NativeTextSelection {
    let start = NativeTextPosition {
        section,
        paragraph,
        scalar_offset: 0,
        cell_path: Vec::new(),
    };
    NativeTextSelection {
        anchor: start.clone(),
        focus: start,
    }
}

/// `picture`, `shape` or `table` for a body-paragraph control, else None.
fn object_kind(
    core: &DocumentCore,
    section: usize,
    paragraph: usize,
    control: usize,
) -> Option<&'static str> {
    match core
        .document()
        .sections
        .get(section)?
        .paragraphs
        .get(paragraph)?
        .controls
        .get(control)?
    {
        Control::Picture(_) => Some("picture"),
        Control::Shape(_) => Some("shape"),
        Control::Table(_) => Some("table"),
        _ => None,
    }
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

    #[test]
    fn styles_list_and_apply_with_undo() {
        let mut editor = editor("제목", selected(0, 0));
        let listed = editor.query(NativeQuery::Styles {}).unwrap();
        let styles = listed["styles"].as_array().unwrap();
        assert!(!styles.is_empty());
        let before = listed["current"].clone();
        let outline = styles
            .iter()
            .find(|style| {
                style["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("개요"))
            })
            .expect("a blank document has outline styles");
        let id = outline["id"].as_u64().unwrap();
        let revision = current_revision(&editor);
        editor
            .apply_action(revision, action(json!({"action": "applyStyle", "id": id})))
            .unwrap();
        assert_eq!(
            editor.query(NativeQuery::Styles {}).unwrap()["current"],
            json!(id)
        );
        let revision = current_revision(&editor);
        editor.undo(revision).unwrap();
        assert_eq!(
            editor.query(NativeQuery::Styles {}).unwrap()["current"],
            before
        );
        assert_eq!(text(&editor), "제목");
    }

    #[test]
    fn two_columns_set_and_read_back() {
        let mut editor = editor("다단", selected(0, 0));
        assert_eq!(
            editor.query(NativeQuery::Columns {}).unwrap()["count"],
            json!(1)
        );
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "setColumns", "count": 2, "spacing": 2268})),
            )
            .unwrap();
        let columns = editor.query(NativeQuery::Columns {}).unwrap();
        assert_eq!(
            (columns["count"].clone(), columns["spacing"].clone()),
            (json!(2), json!(2268))
        );
        let revision = current_revision(&editor);
        editor.undo(revision).unwrap();
        assert_eq!(
            editor.query(NativeQuery::Columns {}).unwrap()["count"],
            json!(1)
        );
    }

    #[test]
    fn document_html_has_the_text_of_every_section() {
        let mut editor = editor("내보낼 글", selected(0, 0));
        let html = editor.query(NativeQuery::DocumentHtml {}).unwrap();
        let sections = html["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 1);
        assert!(sections[0].as_str().unwrap().contains("내보낼 글"));
    }

    /// 1×1 PNG, base64.
    const PNG_BASE64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGMAAQAABQABDQottAAAAABJRU5ErkJggg==";

    #[test]
    fn picture_insert_properties_order_and_delete() {
        let mut editor = editor("그림 앞뒤", selected(2, 2));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "insertPicture", "data": PNG_BASE64, "extension": "png",
                              "width": 7500, "height": 7500, "naturalWidth": 1, "naturalHeight": 1})),
            )
            .unwrap();
        let layout = editor.query(NativeQuery::PageObjects { page: 0 }).unwrap();
        let picture = layout["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|object| object["type"] == json!("image"))
            .expect("the picture is laid out on page 1")
            .clone();
        let address = |key: &str| picture[key].as_u64().unwrap();
        let (section, paragraph, control) =
            (address("secIdx"), address("paraIdx"), address("controlIdx"));
        assert_eq!((section, paragraph), (0, 0));
        // Typing next to the floating picture still works.
        let revision = current_revision(&editor);
        editor.replace_selection(revision, "새").unwrap();
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "setPictureProperties", "section": section, "paragraph": paragraph,
                              "control": control, "props": {"width": 15000, "height": 15000}})),
            )
            .unwrap();
        let props = editor
            .query(NativeQuery::PictureProperties {
                section: section as u32,
                paragraph: paragraph as u32,
                control: control as u32,
            })
            .unwrap();
        assert_eq!(props["width"], json!(15000));
        // Stacking order is for shapes, as in the web editor.
        let revision = current_revision(&editor);
        let refused = editor
            .apply_action(
                revision,
                action(
                    json!({"action": "objectOrder", "section": section, "paragraph": paragraph,
                              "control": control, "operation": "front"}),
                ),
            )
            .unwrap_err();
        assert!(matches!(refused, NativeHostError::Unsupported(_)));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(
                    json!({"action": "deleteObject", "section": section, "paragraph": paragraph,
                              "control": control}),
                ),
            )
            .unwrap();
        let paragraph_controls =
            &editor.session().document().document().sections[0].paragraphs[0].controls;
        assert!(!paragraph_controls
            .iter()
            .any(|c| matches!(c, Control::Picture(_))));
        assert_eq!(text(&editor), "그림새 앞뒤");
    }

    #[test]
    fn page_hide_and_new_page_number_on_the_caret_paragraph() {
        let mut editor = editor("감추기", selected(0, 0));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "setPageHide", "header": true, "footer": false,
                              "masterPage": false, "border": false, "fill": false, "pageNumber": true})),
            )
            .unwrap();
        let hide = editor.query(NativeQuery::PageHide {}).unwrap();
        assert_eq!(hide["exists"], json!(true));
        assert_eq!(hide["hideHeader"], json!(true));
        assert_eq!(hide["hidePageNum"], json!(true));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "newPageNumber", "start": 5})),
            )
            .unwrap();
        let controls = &editor.session().document().document().sections[0].paragraphs[0].controls;
        assert!(controls
            .iter()
            .any(|control| matches!(control, Control::NewNumber(_))));
        // Typing in the paragraph still works next to both controls.
        let revision = current_revision(&editor);
        editor.replace_selection(revision, "가").unwrap();
        assert_eq!(text(&editor), "가감추기");
        assert_eq!(
            editor.query(NativeQuery::FieldInfo {}).unwrap()["inField"],
            json!(false)
        );
    }

    #[test]
    fn shapes_insert_in_front_of_the_text_and_order() {
        let mut editor = editor("도형", selected(1, 1));
        for kind in ["rectangle", "ellipse"] {
            let revision = current_revision(&editor);
            editor
                .apply_action(
                    revision,
                    action(json!({"action": "insertShape", "kind": kind, "width": 8504, "height": 5669})),
                )
                .unwrap();
        }
        let shapes: Vec<Value> = editor.query(NativeQuery::PageObjects { page: 0 }).unwrap()
            ["controls"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|object| object["type"] == json!("shape"))
            .cloned()
            .collect();
        assert_eq!(shapes.len(), 2);
        let first = &shapes[0];
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "objectOrder", "section": first["secIdx"], "paragraph": first["paraIdx"],
                              "control": first["controlIdx"], "operation": "front"})),
            )
            .unwrap();
        let revision = current_revision(&editor);
        editor.replace_selection(revision, "새").unwrap();
        assert_eq!(text(&editor), "도새형");
    }

    #[test]
    fn object_properties_for_shapes_and_captions_for_tables() {
        let mut editor = editor("개체", selected(1, 1));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "insertShape", "kind": "rectangle", "width": 8504, "height": 5669})),
            )
            .unwrap();
        let shape = editor.query(NativeQuery::PageObjects { page: 0 }).unwrap()["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|object| object["type"] == json!("shape"))
            .cloned()
            .unwrap();
        let address = json!({"section": shape["secIdx"], "paragraph": shape["paraIdx"], "control": shape["controlIdx"]});
        let revision = current_revision(&editor);
        let mut request = address.clone();
        request["action"] = json!("setObjectProperties");
        request["props"] = json!({"borderColor": 255, "fillType": "solid", "fillBgColor": 65280, "rotationAngle": 30});
        editor.apply_action(revision, action(request)).unwrap();
        let mut query = address.clone();
        query["kind"] = json!("objectProperties");
        let props = editor
            .query(serde_json::from_value(query).unwrap())
            .unwrap();
        assert_eq!(props["kind"], json!("shape"));
        assert_eq!(props["borderColor"], json!(255));
        assert_eq!(props["fillBgColor"], json!(65280));
        assert_eq!(props["rotationAngle"], json!(30));

        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "insertTable", "rows": 1, "cols": 1})),
            )
            .unwrap();
        let cell = editor.selection().focus.clone();
        let table = json!({"section": cell.section, "paragraph": cell.paragraph,
                           "control": cell.cell_path[0].control});
        let revision = current_revision(&editor);
        let mut request = table.clone();
        request["action"] = json!("setObjectProperties");
        request["props"] = json!({"hasCaption": true});
        editor.apply_action(revision, action(request)).unwrap();
        let mut query = table.clone();
        query["kind"] = json!("objectProperties");
        let props = editor
            .query(serde_json::from_value(query).unwrap())
            .unwrap();
        assert_eq!(props["kind"], json!("table"));
        assert_eq!(props["hasCaption"], json!(true));
        // The caret stays in the cell and typing still works.
        let revision = current_revision(&editor);
        editor.replace_selection(revision, "셀").unwrap();
        assert_eq!(editor.selection().focus.cell_path.len(), 1);
    }

    #[test]
    fn shapes_group_and_ungroup() {
        let mut editor = editor("묶기", selected(1, 1));
        for kind in ["rectangle", "ellipse"] {
            let revision = current_revision(&editor);
            editor
                .apply_action(
                    revision,
                    action(json!({"action": "insertShape", "kind": kind, "width": 8504, "height": 5669})),
                )
                .unwrap();
        }
        let kinds = |editor: &mut NativeEditor| -> Vec<Value> {
            editor.query(NativeQuery::PageObjects { page: 0 }).unwrap()["controls"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|object| {
                    object["type"] == json!("shape") || object["type"] == json!("group")
                })
                .cloned()
                .collect()
        };
        let shapes = kinds(&mut editor);
        assert_eq!(shapes.len(), 2);
        let objects: Vec<Value> = shapes
            .iter()
            .map(|shape| json!({"paragraph": shape["paraIdx"], "control": shape["controlIdx"]}))
            .collect();
        let revision = current_revision(&editor);
        let (_, group) = editor
            .apply_action(
                revision,
                action(json!({"action": "groupObjects", "section": 0, "objects": objects})),
            )
            .unwrap();
        let grouped = kinds(&mut editor);
        assert_eq!(grouped.len(), 1, "{grouped:?}");
        assert_eq!(grouped[0]["type"], json!("group"));
        assert_eq!(grouped[0]["controlIdx"], group["control"]);
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(
                    json!({"action": "ungroupObject", "section": 0, "paragraph": group["paragraph"],
                              "control": group["control"]}),
                ),
            )
            .unwrap();
        assert_eq!(kinds(&mut editor).len(), 2);
        let revision = current_revision(&editor);
        editor.replace_selection(revision, "새").unwrap();
        assert_eq!(text(&editor), "묶새기");
    }

    #[test]
    fn section_settings_and_page_border_round_trip() {
        let mut editor = editor("구역", selected(0, 0));
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(
                    json!({"action": "setSectionDef", "props": {"pageNum": 7, "hideHeader": true}}),
                ),
            )
            .unwrap();
        let section = editor.query(NativeQuery::SectionDef {}).unwrap();
        assert_eq!(section["pageNum"], json!(7));
        assert_eq!(section["hideHeader"], json!(true));
        let line = json!({"type": 1, "width": 1, "color": "#000000"});
        let revision = current_revision(&editor);
        editor
            .apply_action(
                revision,
                action(json!({"action": "setPageBorder", "props": {"borderLeft": line, "borderRight": line,
                              "borderTop": line, "borderBottom": line}})),
            )
            .unwrap();
        let border = editor.query(NativeQuery::PageBorder {}).unwrap();
        assert_eq!(border["borderTop"]["type"], json!(1));
        let revision = current_revision(&editor);
        editor.undo(revision).unwrap();
        assert_eq!(
            editor.query(NativeQuery::PageBorder {}).unwrap()["borderTop"]["type"],
            json!(0)
        );
    }

    #[test]
    fn cell_number_formats() {
        assert_eq!(
            format_cell_number("1234567", "thousands").as_deref(),
            Some("1,234,567")
        );
        assert_eq!(
            format_cell_number("-1,234.5", "thousands").as_deref(),
            Some("-1234.5")
        );
        assert_eq!(
            format_cell_number("12", "addDecimal").as_deref(),
            Some("12.0")
        );
        assert_eq!(
            format_cell_number("1,200.5", "addDecimal").as_deref(),
            Some("1,200.50")
        );
        assert_eq!(
            format_cell_number("3.14", "removeDecimal").as_deref(),
            Some("3.1")
        );
        assert_eq!(
            format_cell_number("3.1", "removeDecimal").as_deref(),
            Some("3")
        );
        assert_eq!(format_cell_number("3", "removeDecimal"), None);
        assert_eq!(format_cell_number("합계", "thousands"), None);
    }
}
