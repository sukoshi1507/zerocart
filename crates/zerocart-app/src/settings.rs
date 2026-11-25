// SPDX-License-Identifier: GPL-3.0-or-later
//! The user's language: remembered in `~/.config/zerocart/lang`, else taken from the system's `LANG`.
use std::path::PathBuf;
use zerocart_ui::Lang;

fn file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/zerocart/lang"))
}

/// The language the user chose last time.
pub fn saved_lang() -> Option<Lang> {
    let text = std::fs::read_to_string(file()?).ok()?;
    let code = text.trim();
    let lang = Lang::from_code(code, Lang::Fr);
    (lang.code() == code).then_some(lang)
}

pub fn save_lang(lang: Lang) {
    #[cfg(not(test))]
    if let Some(f) = file() {
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(f, format!("{}\n", lang.code()));
    }
    #[cfg(test)]
    let _ = lang;
}

/// From a locale such as `ja_JP.UTF-8` or `zh_CN.UTF-8`; `None` for `C`, `POSIX` or anything unknown.
pub fn from_locale(locale: &str) -> Option<Lang> {
    let lang = locale.split(['.', '@']).next()?.replace('_', "-").to_lowercase();
    match lang.split('-').next()? {
        "fr" => Some(Lang::Fr),
        "en" => Some(Lang::En),
        "ja" => Some(Lang::Ja),
        "zh" if lang.contains("tw") || lang.contains("hk") || lang.contains("hant") => None, // traditional is not offered
        "zh" => Some(Lang::Zh),
        _ => None,
    }
}

/// `--lang` first, then the saved choice, then the system's locale, then French.
pub fn initial_lang(flag: Option<&str>) -> Lang {
    if let Some(code) = flag {
        return Lang::from_code(code, Lang::Fr);
    }
    saved_lang()
        .or_else(|| {
            ["LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .filter_map(|v| std::env::var(v).ok())
                .find(|v| !v.is_empty())
                .and_then(|v| from_locale(&v))
        })
        .unwrap_or(Lang::Fr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_map_to_the_four_languages() {
        assert_eq!(from_locale("fr_FR.UTF-8"), Some(Lang::Fr));
        assert_eq!(from_locale("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(from_locale("ja_JP.UTF-8"), Some(Lang::Ja));
        assert_eq!(from_locale("zh_CN.UTF-8"), Some(Lang::Zh));
        assert_eq!(from_locale("zh_TW.UTF-8"), None, "traditional Chinese is not offered");
        assert_eq!(from_locale("C"), None);
        assert_eq!(from_locale("POSIX"), None);
        assert_eq!(from_locale("de_DE"), None);
    }

    #[test]
    fn the_flag_wins_and_an_unknown_code_falls_back_to_french() {
        assert_eq!(initial_lang(Some("ja")), Lang::Ja);
        assert_eq!(initial_lang(Some("zh-cn")), Lang::Zh);
        assert_eq!(initial_lang(Some("xx")), Lang::Fr);
    }
}
