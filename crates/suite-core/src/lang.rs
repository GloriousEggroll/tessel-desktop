//! The suite’s supported interface languages and UTF-8 locales (language_data.py).

/// `(code, native name, locale)` in the Python declaration order.
pub const LANGUAGES: [(&str, &str, &str); 31] = [
    ("en", "English", "en_US.UTF-8"),
    ("zh_CN", "简体中文", "zh_CN.UTF-8"),
    ("zh_TW", "繁體中文", "zh_TW.UTF-8"),
    ("ja", "日本語", "ja_JP.UTF-8"),
    ("ko", "한국어", "ko_KR.UTF-8"),
    ("es", "Español", "es_ES.UTF-8"),
    ("fr", "Français", "fr_FR.UTF-8"),
    ("de", "Deutsch", "de_DE.UTF-8"),
    ("it", "Italiano", "it_IT.UTF-8"),
    ("pt", "Português", "pt_PT.UTF-8"),
    ("pt_BR", "Português (Brasil)", "pt_BR.UTF-8"),
    ("nl", "Nederlands", "nl_NL.UTF-8"),
    ("pl", "Polski", "pl_PL.UTF-8"),
    ("ru", "Русский", "ru_RU.UTF-8"),
    ("uk", "Українська", "uk_UA.UTF-8"),
    ("cs", "Čeština", "cs_CZ.UTF-8"),
    ("sk", "Slovenčina", "sk_SK.UTF-8"),
    ("hu", "Magyar", "hu_HU.UTF-8"),
    ("ro", "Română", "ro_RO.UTF-8"),
    ("el", "Ελληνικά", "el_GR.UTF-8"),
    ("sv", "Svenska", "sv_SE.UTF-8"),
    ("da", "Dansk", "da_DK.UTF-8"),
    ("nb", "Norsk (bokmål)", "nb_NO.UTF-8"),
    ("fi", "Suomi", "fi_FI.UTF-8"),
    ("tr", "Türkçe", "tr_TR.UTF-8"),
    ("ar", "العربية", "ar_EG.UTF-8"),
    ("he", "עברית", "he_IL.UTF-8"),
    ("hi", "हिन्दी", "hi_IN.UTF-8"),
    ("id", "Bahasa Indonesia", "id_ID.UTF-8"),
    ("th", "ไทย", "th_TH.UTF-8"),
    ("vi", "Tiếng Việt", "vi_VN.UTF-8"),
];

pub const RTL_LANGUAGES: [&str; 2] = ["ar", "he"];

/// `code in LANG_NAMES`.
pub fn is_supported(code: &str) -> bool {
    LANGUAGES.iter().any(|(known, ..)| *known == code)
}

/// `LANG_NAMES[code]`.
pub fn language_name(code: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|(known, ..)| *known == code).map(|entry| entry.1)
}

/// `LOCALE_NAMES[code]`.
pub fn locale_name(code: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|(known, ..)| *known == code).map(|entry| entry.2)
}

pub fn is_rtl(code: &str) -> bool {
    RTL_LANGUAGES.contains(&code)
}
