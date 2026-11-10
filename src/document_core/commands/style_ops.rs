//! 스타일 정의 조회·수정·추가·삭제. 웹 래퍼(`wasm_api`)와 네이티브 호스트가 같은
//! 구현을 쓰도록 hwpmac/native-actions 에서 `wasm_api.rs` 의 본문을 옮겨 왔다.

use crate::document_core::helpers::json_escape;
use crate::document_core::DocumentCore;
use crate::model::control::Control;
use crate::model::paragraph::Paragraph;
use crate::renderer::composer::restamp_indentation;

impl DocumentCore {
    /// 특정 스타일의 CharShape/ParaShape 속성을 상세 조회한다.
    ///
    /// 반환값: JSON { charProps: {...}, paraProps: {...} }
    pub fn get_style_detail_native(&self, style_id: u32) -> String {
        let styles = &self.document.doc_info.styles;
        let style = match styles.get(style_id as usize) {
            Some(s) => s,
            None => return "{}".to_string(),
        };
        let char_json = self.build_char_properties_json_by_id(style.char_shape_id);

        // 스타일의 기본 ParaShape에 번호 정보가 없으면,
        // 이 스타일을 사용하는 실제 문단의 ParaShape에서 조회
        let effective_psid =
            self.find_effective_para_shape_for_style(style_id, style.para_shape_id);
        let para_json = self.build_para_properties_json(effective_psid, 0);
        format!(
            "{{\"charProps\":{},\"paraProps\":{}}}",
            char_json, para_json
        )
    }

    /// 스타일의 실효 ParaShape ID를 찾는다.
    /// 스타일 정의의 ParaShape에 번호 정보가 없으면, 이 스타일을 사용하는 문단에서 조회한다.
    fn find_effective_para_shape_for_style(&self, style_id: u32, base_psid: u16) -> u16 {
        use crate::model::style::HeadType;
        // 기본 ParaShape에 이미 번호 정보가 있으면 그대로 사용
        if let Some(ps) = self.document.doc_info.para_shapes.get(base_psid as usize) {
            if ps.head_type != HeadType::None {
                return base_psid;
            }
        }
        // 이 스타일을 사용하는 첫 번째 문단의 para_shape_id에서 번호 정보 탐색
        let sid = style_id as u8;
        for section in &self.document.sections {
            for para in &section.paragraphs {
                if para.style_id == sid {
                    if let Some(ps) = self
                        .document
                        .doc_info
                        .para_shapes
                        .get(para.para_shape_id as usize)
                    {
                        if ps.head_type != HeadType::None {
                            return para.para_shape_id;
                        }
                    }
                }
            }
        }
        base_psid
    }

    /// 스타일의 메타 정보(이름/영문이름/nextStyleId)를 수정한다.
    ///
    /// json: {"name":"...", "englishName":"...", "nextStyleId":0}
    pub fn update_style_native(&mut self, style_id: u32, json: &str) -> bool {
        use crate::document_core::helpers::json_i32;
        let styles = &mut self.document.doc_info.styles;
        let style = match styles.get_mut(style_id as usize) {
            Some(s) => s,
            None => return false,
        };
        // 이름 파싱
        if let Some(name) = crate::document_core::helpers::json_str(json, "name") {
            style.local_name = name;
        }
        if let Some(en) = crate::document_core::helpers::json_str(json, "englishName") {
            style.english_name = en;
        }
        if let Some(v) = json_i32(json, "nextStyleId") {
            style.next_style_id = v as u8;
        }
        // raw_data 무효화 (수정됨)
        style.raw_data = None;
        // DocInfo 스트림 무효화. serialize_doc_info 는 raw_stream_dirty 가 false 이면
        // 원본 스트림을 그대로 반환하고(레코드 raw_data 는 그 이전에 단락됨), 이름/nextStyleId
        // 변경이 .hwp 저장에서 유실된다. 형제 update_style_shapes 는 이미 이 플래그를 세운다.
        self.document.doc_info.raw_stream_dirty = true;
        true
    }

    /// 스타일의 CharShape/ParaShape를 수정한다.
    ///
    /// charMods/paraMods는 기존 parse_char_shape_mods/parse_para_shape_mods와 동일한 JSON 형식
    pub fn update_style_shapes_native(
        &mut self,
        style_id: u32,
        char_mods_json: &str,
        para_mods_json: &str,
    ) -> bool {
        use crate::document_core::helpers::{
            build_tab_def_from_json, json_has_border_keys, json_has_tab_keys,
            parse_char_shape_mods, parse_json_i16_array, parse_para_shape_mods,
        };

        let styles = &self.document.doc_info.styles;
        let style = match styles.get(style_id as usize) {
            Some(s) => s.clone(),
            None => return false,
        };
        let old_csid = style.char_shape_id as u32;
        let old_psid = style.para_shape_id;
        let style_type = style.style_type;

        // CharShape 수정
        if !char_mods_json.is_empty() && char_mods_json != "{}" {
            let mut char_mods = parse_char_shape_mods(char_mods_json);
            // 테두리/배경은 글자 모양 적용 경로처럼 BorderFill 을 찾거나 만들어 단다.
            if json_has_border_keys(char_mods_json) {
                char_mods.border_fill_id = Some(self.create_border_fill_from_json(char_mods_json));
            }
            if let Some(cs) = self
                .document
                .doc_info
                .char_shapes
                .get(style.char_shape_id as usize)
            {
                let new_cs = char_mods.apply_to(cs);
                // 새 CharShape를 추가하고 스타일에 연결
                self.document.doc_info.char_shapes.push(new_cs);
                let new_id = (self.document.doc_info.char_shapes.len() - 1) as u16;
                self.document.doc_info.styles[style_id as usize].char_shape_id = new_id;
            }
        }

        // ParaShape 수정
        if !para_mods_json.is_empty() && para_mods_json != "{}" {
            let mut para_mods = parse_para_shape_mods(para_mods_json);
            // 탭 정의와 테두리/배경은 문단 모양 적용 경로처럼 찾거나 만들어 달고, 테두리 간격도
            // 넣는다. 탭은 스타일의 지금 탭 정의에서 바뀐 항목만 덮어쓴다.
            if json_has_tab_keys(para_mods_json) {
                let doc_info = &self.document.doc_info;
                let base_tab_def_id = doc_info
                    .para_shapes
                    .get(old_psid as usize)
                    .map_or(0, |ps| ps.tab_def_id);
                let tab_def =
                    build_tab_def_from_json(para_mods_json, base_tab_def_id, &doc_info.tab_defs);
                para_mods.tab_def_id = Some(self.document.find_or_create_tab_def(tab_def));
            }
            if json_has_border_keys(para_mods_json) {
                para_mods.border_fill_id = Some(self.create_border_fill_from_json(para_mods_json));
            }
            if let Some(spacing) = parse_json_i16_array(para_mods_json, "borderSpacing", 4) {
                para_mods.border_spacing = Some([spacing[0], spacing[1], spacing[2], spacing[3]]);
            }
            if let Some(ps) = self
                .document
                .doc_info
                .para_shapes
                .get(style.para_shape_id as usize)
            {
                let new_ps = para_mods.apply_to(ps);
                self.document.doc_info.para_shapes.push(new_ps);
                let new_id = (self.document.doc_info.para_shapes.len() - 1) as u16;
                self.document.doc_info.styles[style_id as usize].para_shape_id = new_id;
            }
        }

        // raw_data 무효화
        self.document.doc_info.styles[style_id as usize].raw_data = None;
        self.document.doc_info.raw_stream_dirty = true;

        let sid = style_id as u8;
        let mut body_targets = Vec::new();
        let mut cell_targets = Vec::new();
        for (sec_idx, section) in self.document.sections.iter().enumerate() {
            for (para_idx, para) in section.paragraphs.iter().enumerate() {
                if para.style_id == sid {
                    body_targets.push((sec_idx, para_idx));
                }
                for (control_idx, ctrl) in para.controls.iter().enumerate() {
                    if let Control::Table(table) = ctrl {
                        for (cell_idx, cell) in table.cells.iter().enumerate() {
                            for (cell_para_idx, cpara) in cell.paragraphs.iter().enumerate() {
                                if cpara.style_id == sid {
                                    cell_targets.push((
                                        sec_idx,
                                        para_idx,
                                        control_idx,
                                        cell_idx,
                                        cell_para_idx,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        // ── 스타일 변경을 해당 스타일을 사용하는 모든 문단에 전파 ──
        let updated_style = self.document.doc_info.styles[style_id as usize].clone();
        let new_csid = updated_style.char_shape_id as u32;
        let new_psid = updated_style.para_shape_id;
        // [#7490] 셀 문단은 저장 줄 기록을 두고 재조판하므로 bit 20 도 새 들여쓰기로 단다.
        // 본문 문단은 `reflow_body_paragraph` 가 줄을 비우고 다시 짠다.
        let (old_indent, new_indent) = self.para_shape_indents(old_psid, new_psid);

        for (sec_idx, para_idx) in body_targets {
            if let Some(para) = self
                .document
                .sections
                .get_mut(sec_idx)
                .and_then(|s| s.paragraphs.get_mut(para_idx))
            {
                if style_type == 0 && para.para_shape_id == old_psid {
                    para.para_shape_id = new_psid;
                }
                para.replace_style_char_shape_preserving_overrides(old_csid, new_csid);
            }
            self.reflow_body_paragraph(sec_idx, para_idx);
            if let Some(section) = self.document.sections.get_mut(sec_idx) {
                section.raw_stream = None;
            }
        }

        for (sec_idx, para_idx, control_idx, cell_idx, cell_para_idx) in cell_targets {
            if let Ok(cpara) =
                self.get_cell_paragraph_mut(sec_idx, para_idx, control_idx, cell_idx, cell_para_idx)
            {
                if style_type == 0 && cpara.para_shape_id == old_psid {
                    cpara.para_shape_id = new_psid;
                    restamp_indentation(&mut cpara.line_segs, old_indent, new_indent);
                }
                cpara.replace_style_char_shape_preserving_overrides(old_csid, new_csid);
            }
            self.reflow_cell_paragraph(sec_idx, para_idx, control_idx, cell_idx, cell_para_idx);
            self.mark_cell_control_dirty(sec_idx, para_idx, control_idx);
            if let Some(section) = self.document.sections.get_mut(sec_idx) {
                section.raw_stream = None;
            }
        }

        // 스타일 캐시 무효화 + 전체 리빌드
        let num_sections = self.document.sections.len();
        for sec_idx in 0..num_sections {
            self.rebuild_section(sec_idx);
        }
        true
    }

    /// 새 스타일을 생성한다.
    ///
    /// json: {"name":"...", "englishName":"...", "type":0, "nextStyleId":0, "index":3}
    /// `index`를 주면 그 자리에 끼워 넣고, 그 자리부터 뒤 스타일을 가리키던 모든 참조(문단·덧말·
    /// 바탕쪽·메모·다른 스타일의 다음 스타일)를 한 칸씩 민다. 한컴은 고른 스타일 바로 뒤에 새 스타일을 둔다.
    /// `nextStyleId`는 끼워 넣은 뒤의 번호다. 없으면 목록 끝에 붙인다.
    /// 반환값: 새 스타일 ID (0-based). `index`가 목록 길이보다 크거나, 문단이 스타일 번호를 1바이트로
    /// 저장하므로 256개를 채운 목록 가운데에 끼우면 -1이다.
    pub fn create_style_native(&mut self, json: &str) -> i32 {
        use crate::document_core::helpers::{json_i32, json_str};
        use crate::model::style::Style;

        let count = self.document.doc_info.styles.len();
        let index = match json_i32(json, "index") {
            None => count,
            Some(index) => match usize::try_from(index) {
                Ok(index) if index <= count => index,
                _ => return -1,
            },
        };
        // 문단은 스타일 번호를 1바이트로 저장한다. 256개를 채운 목록 가운데에 끼우면 마지막 스타일이
        // 가리킬 수 없는 256번으로 밀리므로 끼우지 않는다. 끝에 붙이기는 예전처럼 막지 않는다.
        if index < count && count > usize::from(u8::MAX) {
            return -1;
        }
        let name = json_str(json, "name").unwrap_or_default();
        let english_name = json_str(json, "englishName").unwrap_or_default();
        let style_type = json_i32(json, "type").unwrap_or(0) as u8;
        let next_style_id = json_i32(json, "nextStyleId").unwrap_or(0) as u8;

        // 한컴 스타일 추가 흐름은 현재 문단의 모양을 기본값으로 삼는다.
        // 호출자가 base ID를 넘기지 않으면 기존 호환성을 위해 바탕글을 사용한다.
        let base_style = self.document.doc_info.styles.first();
        let (fallback_char_shape_id, fallback_para_shape_id) = match base_style {
            Some(s) => (s.char_shape_id, s.para_shape_id),
            None => (0, 0),
        };
        let char_shape_id = json_i32(json, "baseCharShapeId")
            .filter(|id| *id >= 0)
            .map(|id| id as u16)
            .filter(|id| (*id as usize) < self.document.doc_info.char_shapes.len())
            .unwrap_or(fallback_char_shape_id);
        let para_shape_id = json_i32(json, "baseParaShapeId")
            .filter(|id| *id >= 0)
            .map(|id| id as u16)
            .filter(|id| (*id as usize) < self.document.doc_info.para_shapes.len())
            .unwrap_or(fallback_para_shape_id);

        let new_style = Style {
            raw_data: None,
            local_name: name,
            english_name,
            style_type,
            next_style_id,
            lang_id: 1042, // 한국어 default (HWP5 spec 표 47)
            para_shape_id,
            char_shape_id,
            lock_form: false,
        };
        if index < count {
            self.remap_style_ids(|id| if id >= index as u16 { id + 1 } else { id });
        }
        self.document.doc_info.styles.insert(index, new_style);
        self.document.doc_info.raw_stream_dirty = true;
        // 스타일 캐시 갱신
        self.rebuild_resolved_styles();
        index as i32
    }

    /// 스타일을 삭제한다.
    ///
    /// 바탕글(ID 0)은 삭제할 수 없다.
    /// 삭제된 스타일을 사용 중인 문단은 바탕글(ID 0)로 변경된다.
    pub fn delete_style_native(&mut self, style_id: u32) -> bool {
        if style_id == 0 {
            return false; // 바탕글은 삭제 불가
        }
        let styles = &self.document.doc_info.styles;
        if style_id as usize >= styles.len() {
            return false;
        }
        let sid = style_id as u16;
        self.remap_style_ids(|id| {
            if id == sid {
                0
            } else if id > sid {
                id - 1
            } else {
                id
            }
        });
        // 스타일 삭제 (인덱스 기반이므로 뒤의 ID가 변경됨에 주의)
        self.document.doc_info.styles.remove(style_id as usize);
        // 스타일 캐시 갱신
        self.rebuild_resolved_styles();
        true
    }

    /// 스타일 번호를 가리키는 모든 참조를 `remap`으로 바꾼다. 스타일 목록 자체는 바꾸지 않는다.
    ///
    /// 문단(본문·표·글상자·머리말·주석·메모·바탕쪽·복사해 둔 문단), 덧말, 다른 스타일의 다음 스타일,
    /// 그리고 원본 레코드로 다시 쓰는 바탕쪽·구역 끝 목록(확장 바탕쪽·메모)의 문단 머리를 함께 고친다.
    fn remap_style_ids(&mut self, remap: impl Fn(u16) -> u16) {
        use crate::model::identity::walk::{walk, Node};

        let remap_records = |records: &mut [crate::model::document::RawRecord]| {
            // 저장된 바탕쪽은 원본 레코드를 다시 쓴다. 문단 머리의 스타일 번호(PARA_HEADER 10번째
            // 바이트)도 옮겨야 저장본이 모델과 같다.
            for record in records {
                if record.tag_id == crate::parser::tags::HWPTAG_PARA_HEADER {
                    if let Some(id) = record.data.get_mut(10) {
                        *id = remap(u16::from(*id)) as u8;
                    }
                }
            }
        };
        let remap_paragraphs = |paragraphs: &mut [Paragraph]| {
            walk(paragraphs, |node| {
                match node {
                    Node::Paragraph(para) => para.style_id = remap(u16::from(para.style_id)) as u8,
                    Node::Control(Control::Ruby(ruby)) => {
                        ruby.style_id_ref = remap(ruby.style_id_ref);
                    }
                    Node::Control(Control::SectionDef(section_def)) => {
                        remap_records(&mut section_def.extra_child_records);
                    }
                    _ => {}
                }
                Ok(())
            })
            .expect("style reference traversal is infallible");
        };
        // 구역 끝 목록은 원본 레코드와 모델 투영이 같을 때만 저장한다. 둘을 함께 고쳤으면 다시 봉인한다.
        let tail_sealed = crate::model::section_tail::validate_preservation(&self.document).is_ok();
        // 표·글상자·머리말·주석도 같은 스타일 풀을 참조한다. 모양과 원문은 바꾸지 않는다.
        for section in &mut self.document.sections {
            remap_paragraphs(&mut section.paragraphs);
            remap_records(&mut section.section_def.extra_child_records);
            for master in &mut section.section_def.master_pages {
                remap_paragraphs(&mut master.paragraphs);
            }
            if let Some(tail) = &mut section.trailing_lists {
                remap_records(&mut tail.records);
            }
            // 본문 원본 스트림을 다시 쓰지 않게 해 문단 스타일 번호 변경이 HWP 저장에 남게 한다.
            section.raw_stream = None;
        }
        if tail_sealed {
            self.document.seal_section_tail_provenance();
        }
        // 복사해 둔 문단도 같은 스타일 표를 가리킨다. 붙이면 옮기지 않은 번호의 다른 스타일이 된다.
        if let Some(clipboard) = &mut self.clipboard {
            remap_paragraphs(&mut clipboard.paragraphs);
        }
        if let Some(transpose) = &mut self.table_transpose_clipboard {
            for paragraphs in transpose.data.cells.iter_mut().flatten() {
                remap_paragraphs(paragraphs);
            }
        }
        for s in &mut self.document.doc_info.styles {
            let next = remap(u16::from(s.next_style_id)) as u8;
            if next != s.next_style_id {
                s.next_style_id = next;
                // 원본 레코드 봉인이 없는 기본 문서에서도 이전 연결을 다시 쓰지 않는다.
                s.raw_data = None;
            }
        }
        // 스타일 목록과 문단 스타일 번호가 함께 바뀌므로 DocInfo 원본 스트림도 다시 쓴다.
        self.document.doc_info.raw_stream_dirty = true;
    }
}
