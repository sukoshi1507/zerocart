// SPDX-License-Identifier: MIT
// Copied from ZeroTag (MIT): the language enum only, without the ZeroTag string table.
//! Languages of the interface.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Fr,
    En,
    Ja,
    Zh,
}

pub const LANG_COUNT: usize = 4;

impl Lang {
    pub fn next(self) -> Lang {
        match self {
            Lang::Fr => Lang::En,
            Lang::En => Lang::Ja,
            Lang::Ja => Lang::Zh,
            Lang::Zh => Lang::Fr,
        }
    }

    /// Position in the cycle of [`Lang::next`] (for sharing the choice between threads as a number).
    pub fn index(self) -> u8 {
        match self {
            Lang::Fr => 0,
            Lang::En => 1,
            Lang::Ja => 2,
            Lang::Zh => 3,
        }
    }

    pub fn from_index(i: u8) -> Lang {
        match i {
            1 => Lang::En,
            2 => Lang::Ja,
            3 => Lang::Zh,
            _ => Lang::Fr,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::Fr => "fr",
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::Zh => "zh",
        }
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Fr => "Français",
            Lang::En => "English",
            Lang::Ja => "日本語",
            Lang::Zh => "简体中文",
        }
    }

    /// Which face of the Noto Sans CJK collection draws this language (Han glyphs differ: JP = 0, SC = 2).
    pub fn cjk_face(self) -> u32 {
        match self {
            Lang::Zh => 2,
            _ => 0,
        }
    }

    /// Unknown codes give `fallback`; "jp" is accepted for Japanese, "zh-cn" and friends for Simplified Chinese.
    pub fn from_code(code: &str, fallback: Lang) -> Lang {
        match code {
            "fr" => Lang::Fr,
            "en" => Lang::En,
            "ja" | "jp" => Lang::Ja,
            "zh" | "zh-cn" | "zh_cn" | "zh-hans" => Lang::Zh,
            _ => fallback,
        }
    }
}
