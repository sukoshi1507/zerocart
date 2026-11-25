// SPDX-License-Identifier: GPL-3.0-or-later
//! Localised texts: every message is written once per language, next to the code that shows it, and the macro makes
//! the compiler refuse a message that lacks a language.
//!
//! ```ignore
//! let s: &str = loc!(lang, fr: "Lire la ROM", en: "Read ROM", ja: "ROMを読む", zh: "读取ROM");
//! let s: String = loc!(lang, fr: format!("{n} secteurs"), en: format!("{n} sectors"), ja: format!("{n}セクター"), zh: format!("{n}个扇区"));
//! ```

#[macro_export]
macro_rules! loc {
    ($lang:expr, fr: $fr:expr, en: $en:expr, ja: $ja:expr, zh: $zh:expr $(,)?) => {
        match $lang {
            $crate::Lang::Fr => $fr,
            $crate::Lang::En => $en,
            $crate::Lang::Ja => $ja,
            $crate::Lang::Zh => $zh,
        }
    };
}

use crate::Lang;

/// Words that the key legends repeat on many screens.
pub mod word {
    use super::Lang;

    pub fn pick(l: Lang) -> &'static str {
        loc!(l, fr: "choisir", en: "pick", ja: "選択", zh: "选择")
    }
    pub fn back(l: Lang) -> &'static str {
        loc!(l, fr: "retour", en: "back", ja: "戻る", zh: "返回")
    }
    pub fn cancel(l: Lang) -> &'static str {
        loc!(l, fr: "annuler", en: "cancel", ja: "中止", zh: "取消")
    }
    pub fn confirm(l: Lang) -> &'static str {
        loc!(l, fr: "confirmer", en: "confirm", ja: "確定", zh: "确认")
    }
    pub fn quit(l: Lang) -> &'static str {
        loc!(l, fr: "quitter", en: "quit", ja: "終了", zh: "退出")
    }
    pub fn send(l: Lang) -> &'static str {
        loc!(l, fr: "envoyer", en: "send", ja: "送信", zh: "发送")
    }
}
