//! 기본 문단 머리 정의: 문단 번호(7수준 한컴 기본 형식)와 글머리표.
//!
//! 웹 편집기(`ensureDefaultNumbering`·`ensureDefaultBullet`)와 네이티브 편집기가
//! 같은 정의를 쓰도록 `wasm_api`에서 옮겨 왔다.

use crate::document_core::DocumentCore;

impl DocumentCore {
    /// 문서에 기본 문단 번호 정의가 없으면 생성한다.
    ///
    /// 반환값: Numbering ID (1-based)
    pub fn ensure_default_numbering_native(&mut self) -> u16 {
        let numberings = &self.document.doc_info.numberings;
        if !numberings.is_empty() {
            return 1; // 이미 있으면 첫 번째 반환
        }
        // 기본 7수준 번호 형식 생성 (한컴 기본 패턴)
        use crate::model::style::{Numbering, NumberingHead};
        let mut n = Numbering::default();
        // 1. 가) (1) (가) ① ㄱ) a) — 8~10수준은 비워 둔다.
        let texts = ["^1.", "^2)", "^3)", "^4)", "^5)", "^6)", "^7)"];
        for (format, text) in n.level_formats.iter_mut().zip(texts) {
            *format = text.to_string();
        }
        n.start_number = 1;
        n.level_start_numbers = [1; crate::model::style::NUMBERING_LEVELS];
        // 수준별 번호 형식 코드 설정
        // [#7418] 한/글 기본 머리 모양은 자동 내어쓰기(속성 bit3)다 — 둘째 줄부터 본문 시작에 맞춘다.
        n.heads[0] = NumberingHead {
            number_format: 0,
            attr: 1 << 3,
            ..Default::default()
        }; // 1,2,3
        n.heads[1] = NumberingHead {
            number_format: 8,
            attr: 1 << 3,
            ..Default::default()
        }; // 가,나,다
        n.heads[2] = NumberingHead {
            number_format: 0,
            attr: 1 << 3,
            ..Default::default()
        }; // 1,2,3
        n.heads[3] = NumberingHead {
            number_format: 8,
            attr: 1 << 3,
            ..Default::default()
        }; // 가,나,다
        n.heads[4] = NumberingHead {
            number_format: 1,
            attr: 1 << 3,
            ..Default::default()
        }; // ①②③
        n.heads[5] = NumberingHead {
            number_format: 10,
            attr: 1 << 3,
            ..Default::default()
        }; // ㄱ,ㄴ,ㄷ
        n.heads[6] = NumberingHead {
            number_format: 5,
            attr: 1 << 3,
            ..Default::default()
        }; // a,b,c
        self.document.doc_info.numberings.push(n);
        // 새 번호 정의는 DocInfo 원본 스트림에 없어 HWP 저장이 모델에서 다시 쓰게 한다.
        self.document.doc_info.raw_stream_dirty = true;
        1
    }

    /// 글머리표 문자의 정의를 찾고, 없으면 기본 머리 모양으로 만든다.
    ///
    /// 반환값: Bullet ID (1-based)
    pub fn ensure_default_bullet_native(&mut self, bullet_char_str: &str) -> u16 {
        let bullet_ch = bullet_char_str.chars().next().unwrap_or('●');
        // 이미 해당 문자의 Bullet이 있는지 검색
        let bullets = &self.document.doc_info.bullets;
        for (i, b) in bullets.iter().enumerate() {
            let mapped = crate::renderer::layout::map_pua_bullet_char(b.bullet_char);
            if mapped == bullet_ch {
                return (i + 1) as u16;
            }
        }
        // 없으면 새로 생성
        use crate::model::style::Bullet;
        let b = Bullet {
            bullet_char: bullet_ch,
            attr: 1 << 3, // 자동 내어쓰기 (한/글 기본 머리 모양)
            text_distance: 50,
            ..Default::default()
        };
        self.document.doc_info.bullets.push(b);
        // 새 정의는 DocInfo 원본 스트림에 없다 — `bullet_create`와 같이 HWP 저장이 모델에서 다시 쓰게 한다.
        self.document.doc_info.raw_stream_dirty = true;
        self.document.doc_info.bullets.len() as u16
    }
}
