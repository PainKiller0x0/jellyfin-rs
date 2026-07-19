use axum::{Json, response::IntoResponse};
use serde_json::{Value, json};

pub async fn localization_options() -> impl IntoResponse {
    Json(
        LOCALIZATION_OPTIONS
            .iter()
            .map(|(name, value)| json!({ "Name": name, "Value": value }))
            .collect::<Vec<Value>>(),
    )
}

pub async fn localization_cultures() -> impl IntoResponse {
    let mut cultures = CULTURES
        .iter()
        .map(|culture| {
            json!({
                "Name": culture.name,
                "DisplayName": culture.display_name,
                "TwoLetterISOLanguageName": culture.two_letter,
                "ThreeLetterISOLanguageName": culture.three_letter.first().copied(),
                "ThreeLetterISOLanguageNames": culture.three_letter,
            })
        })
        .collect::<Vec<Value>>();
    cultures.sort_by_key(json_name);
    Json(cultures)
}

pub async fn localization_countries() -> impl IntoResponse {
    Json(
        COUNTRIES
            .iter()
            .map(|country| {
                json!({
                    "Name": country.name,
                    "DisplayName": country.display_name,
                    "TwoLetterISORegionName": country.two_letter,
                    "ThreeLetterISORegionName": country.three_letter,
                })
            })
            .collect::<Vec<Value>>(),
    )
}

pub async fn parental_ratings() -> impl IntoResponse {
    Json(
        PARENTAL_RATINGS
            .iter()
            .map(|rating| {
                json!({
                    "Name": rating.name,
                    "Value": rating.score.unwrap_or(-1),
                })
            })
            .collect::<Vec<Value>>(),
    )
}

fn json_name(value: &Value) -> String {
    value
        .get("DisplayName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

struct CultureInfo {
    name: &'static str,
    display_name: &'static str,
    two_letter: &'static str,
    three_letter: &'static [&'static str],
}

struct CountryInfo {
    name: &'static str,
    display_name: &'static str,
    two_letter: &'static str,
    three_letter: &'static str,
}

struct RatingInfo {
    name: &'static str,
    score: Option<i64>,
}

const LOCALIZATION_OPTIONS: &[(&str, &str)] = &[
    ("Afrikaans", "af"),
    ("العربية", "ar"),
    ("Беларуская", "be"),
    ("Български", "bg-BG"),
    ("বাংলা (বাংলাদেশ)", "bn"),
    ("Català", "ca"),
    ("Čeština", "cs"),
    ("Cymraeg", "cy"),
    ("Dansk", "da"),
    ("Deutsch", "de"),
    ("English (United Kingdom)", "en-GB"),
    ("English", "en-US"),
    ("Ελληνικά", "el"),
    ("Esperanto", "eo"),
    ("Español", "es"),
    ("Español americano", "es_419"),
    ("Español (Argentina)", "es-AR"),
    ("Español (Dominicana)", "es_DO"),
    ("Español (México)", "es-MX"),
    ("Eesti", "et"),
    ("Basque", "eu"),
    ("فارسی", "fa"),
    ("Suomi", "fi"),
    ("Filipino", "fil"),
    ("Français", "fr"),
    ("Français (Canada)", "fr-CA"),
    ("Galego", "gl"),
    ("Schwiizerdütsch", "gsw"),
    ("עִבְרִית", "he"),
    ("हिन्दी", "hi"),
    ("Hrvatski", "hr"),
    ("Magyar", "hu"),
    ("Bahasa Indonesia", "id"),
    ("Íslenska", "is"),
    ("Italiano", "it"),
    ("日本語", "ja"),
    ("Qazaqşa", "kk"),
    ("한국어", "ko"),
    ("Lietuvių", "lt"),
    ("Latviešu", "lv"),
    ("Македонски", "mk"),
    ("മലയാളം", "ml"),
    ("मराठी", "mr"),
    ("Bahasa Melayu", "ms"),
    ("Norsk bokmål", "nb"),
    ("नेपाली", "ne"),
    ("Nederlands", "nl"),
    ("Norsk nynorsk", "nn"),
    ("ਪੰਜਾਬੀ", "pa"),
    ("Polski", "pl"),
    ("Pirate", "pr"),
    ("Português", "pt"),
    ("Português (Brasil)", "pt-BR"),
    ("Português (Portugal)", "pt-PT"),
    ("Românește", "ro"),
    ("Русский", "ru"),
    ("Slovenčina", "sk"),
    ("Slovenščina", "sl-SI"),
    ("Shqip", "sq"),
    ("Српски", "sr"),
    ("Svenska", "sv"),
    ("தமிழ்", "ta"),
    ("తెలుగు", "te"),
    ("ภาษาไทย", "th"),
    ("Türkçe", "tr"),
    ("Українська", "uk"),
    ("اُردُو", "ur_PK"),
    ("Tiếng Việt", "vi"),
    ("汉语 (简体字)", "zh-CN"),
    ("漢語 (繁體字)", "zh-TW"),
    ("廣東話 (香港)", "zh-HK"),
];

const CULTURES: &[CultureInfo] = &[
    CultureInfo {
        name: "af",
        display_name: "Afrikaans",
        two_letter: "af",
        three_letter: &["afr"],
    },
    CultureInfo {
        name: "ar",
        display_name: "Arabic",
        two_letter: "ar",
        three_letter: &["ara"],
    },
    CultureInfo {
        name: "bg-BG",
        display_name: "Bulgarian",
        two_letter: "bg",
        three_letter: &["bul"],
    },
    CultureInfo {
        name: "bn",
        display_name: "Bangla",
        two_letter: "bn",
        three_letter: &["ben"],
    },
    CultureInfo {
        name: "ca",
        display_name: "Catalan",
        two_letter: "ca",
        three_letter: &["cat"],
    },
    CultureInfo {
        name: "cs",
        display_name: "Czech",
        two_letter: "cs",
        three_letter: &["ces", "cze"],
    },
    CultureInfo {
        name: "cy",
        display_name: "Welsh",
        two_letter: "cy",
        three_letter: &["cym", "wel"],
    },
    CultureInfo {
        name: "da",
        display_name: "Danish",
        two_letter: "da",
        three_letter: &["dan"],
    },
    CultureInfo {
        name: "de",
        display_name: "German",
        two_letter: "de",
        three_letter: &["deu", "ger"],
    },
    CultureInfo {
        name: "el",
        display_name: "Greek",
        two_letter: "el",
        three_letter: &["ell", "gre"],
    },
    CultureInfo {
        name: "en-GB",
        display_name: "English (United Kingdom)",
        two_letter: "en",
        three_letter: &["eng"],
    },
    CultureInfo {
        name: "en-US",
        display_name: "English",
        two_letter: "en",
        three_letter: &["eng"],
    },
    CultureInfo {
        name: "eo",
        display_name: "Esperanto",
        two_letter: "eo",
        three_letter: &["epo"],
    },
    CultureInfo {
        name: "es",
        display_name: "Spanish",
        two_letter: "es",
        three_letter: &["spa"],
    },
    CultureInfo {
        name: "es-AR",
        display_name: "Spanish (Argentina)",
        two_letter: "es",
        three_letter: &["spa"],
    },
    CultureInfo {
        name: "es-MX",
        display_name: "Spanish (Mexico)",
        two_letter: "es",
        three_letter: &["spa"],
    },
    CultureInfo {
        name: "et",
        display_name: "Estonian",
        two_letter: "et",
        three_letter: &["est"],
    },
    CultureInfo {
        name: "eu",
        display_name: "Basque",
        two_letter: "eu",
        three_letter: &["eus", "baq"],
    },
    CultureInfo {
        name: "fa",
        display_name: "Persian",
        two_letter: "fa",
        three_letter: &["fas", "per"],
    },
    CultureInfo {
        name: "fi",
        display_name: "Finnish",
        two_letter: "fi",
        three_letter: &["fin"],
    },
    CultureInfo {
        name: "fil",
        display_name: "Filipino",
        two_letter: "fil",
        three_letter: &["fil"],
    },
    CultureInfo {
        name: "fr",
        display_name: "French",
        two_letter: "fr",
        three_letter: &["fra", "fre"],
    },
    CultureInfo {
        name: "fr-CA",
        display_name: "French (Canada)",
        two_letter: "fr",
        three_letter: &["fra", "fre"],
    },
    CultureInfo {
        name: "gl",
        display_name: "Galician",
        two_letter: "gl",
        three_letter: &["glg"],
    },
    CultureInfo {
        name: "he",
        display_name: "Hebrew",
        two_letter: "he",
        three_letter: &["heb"],
    },
    CultureInfo {
        name: "hi",
        display_name: "Hindi",
        two_letter: "hi",
        three_letter: &["hin"],
    },
    CultureInfo {
        name: "hr",
        display_name: "Croatian",
        two_letter: "hr",
        three_letter: &["hrv"],
    },
    CultureInfo {
        name: "hu",
        display_name: "Hungarian",
        two_letter: "hu",
        three_letter: &["hun"],
    },
    CultureInfo {
        name: "id",
        display_name: "Indonesian",
        two_letter: "id",
        three_letter: &["ind"],
    },
    CultureInfo {
        name: "is",
        display_name: "Icelandic",
        two_letter: "is",
        three_letter: &["isl", "ice"],
    },
    CultureInfo {
        name: "it",
        display_name: "Italian",
        two_letter: "it",
        three_letter: &["ita"],
    },
    CultureInfo {
        name: "ja",
        display_name: "Japanese",
        two_letter: "ja",
        three_letter: &["jpn"],
    },
    CultureInfo {
        name: "kk",
        display_name: "Kazakh",
        two_letter: "kk",
        three_letter: &["kaz"],
    },
    CultureInfo {
        name: "ko",
        display_name: "Korean",
        two_letter: "ko",
        three_letter: &["kor"],
    },
    CultureInfo {
        name: "lt",
        display_name: "Lithuanian",
        two_letter: "lt",
        three_letter: &["lit"],
    },
    CultureInfo {
        name: "lv",
        display_name: "Latvian",
        two_letter: "lv",
        three_letter: &["lav"],
    },
    CultureInfo {
        name: "mk",
        display_name: "Macedonian",
        two_letter: "mk",
        three_letter: &["mkd", "mac"],
    },
    CultureInfo {
        name: "ml",
        display_name: "Malayalam",
        two_letter: "ml",
        three_letter: &["mal"],
    },
    CultureInfo {
        name: "mr",
        display_name: "Marathi",
        two_letter: "mr",
        three_letter: &["mar"],
    },
    CultureInfo {
        name: "ms",
        display_name: "Malay",
        two_letter: "ms",
        three_letter: &["msa", "may"],
    },
    CultureInfo {
        name: "nb",
        display_name: "Norwegian Bokmal",
        two_letter: "nb",
        three_letter: &["nob"],
    },
    CultureInfo {
        name: "ne",
        display_name: "Nepali",
        two_letter: "ne",
        three_letter: &["nep"],
    },
    CultureInfo {
        name: "nl",
        display_name: "Dutch",
        two_letter: "nl",
        three_letter: &["nld", "dut"],
    },
    CultureInfo {
        name: "nn",
        display_name: "Norwegian Nynorsk",
        two_letter: "nn",
        three_letter: &["nno"],
    },
    CultureInfo {
        name: "pa",
        display_name: "Punjabi",
        two_letter: "pa",
        three_letter: &["pan"],
    },
    CultureInfo {
        name: "pl",
        display_name: "Polish",
        two_letter: "pl",
        three_letter: &["pol"],
    },
    CultureInfo {
        name: "pt",
        display_name: "Portuguese",
        two_letter: "pt",
        three_letter: &["por"],
    },
    CultureInfo {
        name: "pt-BR",
        display_name: "Portuguese (Brazil)",
        two_letter: "pt",
        three_letter: &["por"],
    },
    CultureInfo {
        name: "pt-PT",
        display_name: "Portuguese (Portugal)",
        two_letter: "pt",
        three_letter: &["por"],
    },
    CultureInfo {
        name: "ro",
        display_name: "Romanian",
        two_letter: "ro",
        three_letter: &["ron", "rum"],
    },
    CultureInfo {
        name: "ru",
        display_name: "Russian",
        two_letter: "ru",
        three_letter: &["rus"],
    },
    CultureInfo {
        name: "sk",
        display_name: "Slovak",
        two_letter: "sk",
        three_letter: &["slk", "slo"],
    },
    CultureInfo {
        name: "sl-SI",
        display_name: "Slovenian",
        two_letter: "sl",
        three_letter: &["slv"],
    },
    CultureInfo {
        name: "sq",
        display_name: "Albanian",
        two_letter: "sq",
        three_letter: &["sqi", "alb"],
    },
    CultureInfo {
        name: "sr",
        display_name: "Serbian",
        two_letter: "sr",
        three_letter: &["srp"],
    },
    CultureInfo {
        name: "sv",
        display_name: "Swedish",
        two_letter: "sv",
        three_letter: &["swe"],
    },
    CultureInfo {
        name: "ta",
        display_name: "Tamil",
        two_letter: "ta",
        three_letter: &["tam"],
    },
    CultureInfo {
        name: "te",
        display_name: "Telugu",
        two_letter: "te",
        three_letter: &["tel"],
    },
    CultureInfo {
        name: "th",
        display_name: "Thai",
        two_letter: "th",
        three_letter: &["tha"],
    },
    CultureInfo {
        name: "tr",
        display_name: "Turkish",
        two_letter: "tr",
        three_letter: &["tur"],
    },
    CultureInfo {
        name: "uk",
        display_name: "Ukrainian",
        two_letter: "uk",
        three_letter: &["ukr"],
    },
    CultureInfo {
        name: "ur_PK",
        display_name: "Urdu",
        two_letter: "ur",
        three_letter: &["urd"],
    },
    CultureInfo {
        name: "vi",
        display_name: "Vietnamese",
        two_letter: "vi",
        three_letter: &["vie"],
    },
    CultureInfo {
        name: "zh-CN",
        display_name: "Chinese (Simplified)",
        two_letter: "zh",
        three_letter: &["zho", "chi"],
    },
    CultureInfo {
        name: "zh-HK",
        display_name: "Chinese (Hong Kong)",
        two_letter: "zh",
        three_letter: &["zho", "chi"],
    },
    CultureInfo {
        name: "zh-TW",
        display_name: "Chinese (Traditional)",
        two_letter: "zh",
        three_letter: &["zho", "chi"],
    },
];

const COUNTRIES: &[CountryInfo] = &[
    CountryInfo {
        name: "Argentina",
        display_name: "Argentina",
        two_letter: "AR",
        three_letter: "ARG",
    },
    CountryInfo {
        name: "Australia",
        display_name: "Australia",
        two_letter: "AU",
        three_letter: "AUS",
    },
    CountryInfo {
        name: "Austria",
        display_name: "Austria",
        two_letter: "AT",
        three_letter: "AUT",
    },
    CountryInfo {
        name: "Belgium",
        display_name: "Belgium",
        two_letter: "BE",
        three_letter: "BEL",
    },
    CountryInfo {
        name: "Brazil",
        display_name: "Brazil",
        two_letter: "BR",
        three_letter: "BRA",
    },
    CountryInfo {
        name: "Bulgaria",
        display_name: "Bulgaria",
        two_letter: "BG",
        three_letter: "BGR",
    },
    CountryInfo {
        name: "Canada",
        display_name: "Canada",
        two_letter: "CA",
        three_letter: "CAN",
    },
    CountryInfo {
        name: "Chile",
        display_name: "Chile",
        two_letter: "CL",
        three_letter: "CHL",
    },
    CountryInfo {
        name: "China",
        display_name: "China",
        two_letter: "CN",
        three_letter: "CHN",
    },
    CountryInfo {
        name: "Colombia",
        display_name: "Colombia",
        two_letter: "CO",
        three_letter: "COL",
    },
    CountryInfo {
        name: "Croatia",
        display_name: "Croatia",
        two_letter: "HR",
        three_letter: "HRV",
    },
    CountryInfo {
        name: "Czechia",
        display_name: "Czechia",
        two_letter: "CZ",
        three_letter: "CZE",
    },
    CountryInfo {
        name: "Denmark",
        display_name: "Denmark",
        two_letter: "DK",
        three_letter: "DNK",
    },
    CountryInfo {
        name: "Estonia",
        display_name: "Estonia",
        two_letter: "EE",
        three_letter: "EST",
    },
    CountryInfo {
        name: "Finland",
        display_name: "Finland",
        two_letter: "FI",
        three_letter: "FIN",
    },
    CountryInfo {
        name: "France",
        display_name: "France",
        two_letter: "FR",
        three_letter: "FRA",
    },
    CountryInfo {
        name: "Germany",
        display_name: "Germany",
        two_letter: "DE",
        three_letter: "DEU",
    },
    CountryInfo {
        name: "Greece",
        display_name: "Greece",
        two_letter: "GR",
        three_letter: "GRC",
    },
    CountryInfo {
        name: "Hong Kong",
        display_name: "Hong Kong",
        two_letter: "HK",
        three_letter: "HKG",
    },
    CountryInfo {
        name: "Hungary",
        display_name: "Hungary",
        two_letter: "HU",
        three_letter: "HUN",
    },
    CountryInfo {
        name: "Iceland",
        display_name: "Iceland",
        two_letter: "IS",
        three_letter: "ISL",
    },
    CountryInfo {
        name: "India",
        display_name: "India",
        two_letter: "IN",
        three_letter: "IND",
    },
    CountryInfo {
        name: "Indonesia",
        display_name: "Indonesia",
        two_letter: "ID",
        three_letter: "IDN",
    },
    CountryInfo {
        name: "Ireland",
        display_name: "Ireland",
        two_letter: "IE",
        three_letter: "IRL",
    },
    CountryInfo {
        name: "Israel",
        display_name: "Israel",
        two_letter: "IL",
        three_letter: "ISR",
    },
    CountryInfo {
        name: "Italy",
        display_name: "Italy",
        two_letter: "IT",
        three_letter: "ITA",
    },
    CountryInfo {
        name: "Japan",
        display_name: "Japan",
        two_letter: "JP",
        three_letter: "JPN",
    },
    CountryInfo {
        name: "Korea",
        display_name: "Korea",
        two_letter: "KR",
        three_letter: "KOR",
    },
    CountryInfo {
        name: "Latvia",
        display_name: "Latvia",
        two_letter: "LV",
        three_letter: "LVA",
    },
    CountryInfo {
        name: "Lithuania",
        display_name: "Lithuania",
        two_letter: "LT",
        three_letter: "LTU",
    },
    CountryInfo {
        name: "Malaysia",
        display_name: "Malaysia",
        two_letter: "MY",
        three_letter: "MYS",
    },
    CountryInfo {
        name: "Mexico",
        display_name: "Mexico",
        two_letter: "MX",
        three_letter: "MEX",
    },
    CountryInfo {
        name: "Netherlands",
        display_name: "Netherlands",
        two_letter: "NL",
        three_letter: "NLD",
    },
    CountryInfo {
        name: "New Zealand",
        display_name: "New Zealand",
        two_letter: "NZ",
        three_letter: "NZL",
    },
    CountryInfo {
        name: "Norway",
        display_name: "Norway",
        two_letter: "NO",
        three_letter: "NOR",
    },
    CountryInfo {
        name: "Philippines",
        display_name: "Philippines",
        two_letter: "PH",
        three_letter: "PHL",
    },
    CountryInfo {
        name: "Poland",
        display_name: "Poland",
        two_letter: "PL",
        three_letter: "POL",
    },
    CountryInfo {
        name: "Portugal",
        display_name: "Portugal",
        two_letter: "PT",
        three_letter: "PRT",
    },
    CountryInfo {
        name: "Romania",
        display_name: "Romania",
        two_letter: "RO",
        three_letter: "ROU",
    },
    CountryInfo {
        name: "Russia",
        display_name: "Russia",
        two_letter: "RU",
        three_letter: "RUS",
    },
    CountryInfo {
        name: "Singapore",
        display_name: "Singapore",
        two_letter: "SG",
        three_letter: "SGP",
    },
    CountryInfo {
        name: "Slovakia",
        display_name: "Slovakia",
        two_letter: "SK",
        three_letter: "SVK",
    },
    CountryInfo {
        name: "Slovenia",
        display_name: "Slovenia",
        two_letter: "SI",
        three_letter: "SVN",
    },
    CountryInfo {
        name: "South Africa",
        display_name: "South Africa",
        two_letter: "ZA",
        three_letter: "ZAF",
    },
    CountryInfo {
        name: "Spain",
        display_name: "Spain",
        two_letter: "ES",
        three_letter: "ESP",
    },
    CountryInfo {
        name: "Sweden",
        display_name: "Sweden",
        two_letter: "SE",
        three_letter: "SWE",
    },
    CountryInfo {
        name: "Switzerland",
        display_name: "Switzerland",
        two_letter: "CH",
        three_letter: "CHE",
    },
    CountryInfo {
        name: "Taiwan",
        display_name: "Taiwan",
        two_letter: "TW",
        three_letter: "TWN",
    },
    CountryInfo {
        name: "Thailand",
        display_name: "Thailand",
        two_letter: "TH",
        three_letter: "THA",
    },
    CountryInfo {
        name: "Turkey",
        display_name: "Turkey",
        two_letter: "TR",
        three_letter: "TUR",
    },
    CountryInfo {
        name: "Ukraine",
        display_name: "Ukraine",
        two_letter: "UA",
        three_letter: "UKR",
    },
    CountryInfo {
        name: "United Kingdom",
        display_name: "United Kingdom",
        two_letter: "GB",
        three_letter: "GBR",
    },
    CountryInfo {
        name: "United States",
        display_name: "United States",
        two_letter: "US",
        three_letter: "USA",
    },
    CountryInfo {
        name: "Vietnam",
        display_name: "Vietnam",
        two_letter: "VN",
        three_letter: "VNM",
    },
];

const PARENTAL_RATINGS: &[RatingInfo] = &[
    RatingInfo {
        name: "Unrated",
        score: None,
    },
    RatingInfo {
        name: "Approved",
        score: Some(0),
    },
    RatingInfo {
        name: "G",
        score: Some(1),
    },
    RatingInfo {
        name: "TV-Y",
        score: Some(2),
    },
    RatingInfo {
        name: "TV-Y7",
        score: Some(3),
    },
    RatingInfo {
        name: "PG",
        score: Some(10),
    },
    RatingInfo {
        name: "TV-PG",
        score: Some(10),
    },
    RatingInfo {
        name: "PG-13",
        score: Some(13),
    },
    RatingInfo {
        name: "TV-14",
        score: Some(14),
    },
    RatingInfo {
        name: "R",
        score: Some(17),
    },
    RatingInfo {
        name: "NC-17",
        score: Some(18),
    },
    RatingInfo {
        name: "TV-MA",
        score: Some(18),
    },
    RatingInfo {
        name: "21",
        score: Some(21),
    },
    RatingInfo {
        name: "XXX",
        score: Some(1000),
    },
    RatingInfo {
        name: "Banned",
        score: Some(1001),
    },
];
