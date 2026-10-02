use std::{borrow::Cow, sync::LazyLock};

use regex::Regex;

/// Serial normalization matching shiragame's `serial.normalized` column.
/// https://github.com/SnowflakePowered/shiratsu/blob/master/shiratsu-dat/src/common/serial.rs
pub struct Serial;

static SONY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<code>[a-zA-Z]+)[-_ ](?P<number>\d+)([#-_ /]*(\w?|$))*$").unwrap()
});
static SEGA: LazyLock<[(Regex, &str); 3]> = LazyLock::new(|| {
    [
        (
            Regex::new(r"^(?P<pre>[\d\w]+)-(?P<code>[\d\w]+)(-[\w\d.]+)$").unwrap(),
            "$pre-$code",
        ),
        (
            Regex::new(r"^(?P<pre>MK|T|GS)(?P<code>[\d\w]+)(-[\w\d.]+)?$").unwrap(),
            "$pre-$code",
        ),
        (
            Regex::new(r"^(?P<pre>0{2,3})(?P<code>[\d]+)(-\d{2}\w?)?$").unwrap(),
            "$pre$code",
        ),
    ]
});
static NEC_TGCD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?P<code>[\d\w]{4,5})[ -](?P<number>[\d\w]+)$").unwrap());
static NINTENDO: LazyLock<[(&str, Regex); 5]> = LazyLock::new(|| {
    [
        ("NINTENDO_GCN", r"^DL-DOL-(?P<code>[\w]{4})-[-\w\(\)]+$"),
        ("NINTENDO_WII", r"^RVL-(?P<code>[\w]{4})-[-\w\(\)]+$"),
        (
            "NINTENDO_WIIU",
            r"^WUP-(P|M|N|T|U|B)-(?P<code>[\w]{4})-[-\w\(\)]+$",
        ),
        (
            "NINTENDO_3DS",
            r"^CTR-(P|M|N|T|U|B)-(?P<code>[\w]{4})(-[-\w\(\)]+)*$",
        ),
        ("NINTENDO_NSW", r"^LA-H-(?P<code>[\w]{5})(-[-\w\(\)]+)*$"),
    ]
    .map(|(platform, pattern)| (platform, Regex::new(pattern).unwrap()))
});

impl Serial {
    pub fn normalize<'a>(platform: &str, serial: &'a str) -> Cow<'a, str> {
        match platform {
            "SONY_PSX" | "SONY_PS2" | "SONY_PS3" | "SONY_PS4" | "SONY_PSP" | "SONY_PSV" => {
                SONY.replace(serial, "$code-$number")
            }
            "SEGA_GEN" | "SEGA_CD" | "SEGA_DC" | "SEGA_GG" | "SEGA_SAT" | "SEGA_32X"
            | "SEGA_32X_CD" => SEGA
                .iter()
                .find(|(rule, _)| rule.is_match(serial))
                .map_or(Cow::Borrowed(serial), |(rule, replacement)| {
                    rule.replace(serial, *replacement)
                }),
            "NEC_TGCD" => NEC_TGCD.replace(serial, "$code$number"),
            platform => NINTENDO
                .iter()
                .find(|(nintendo, _)| *nintendo == platform)
                .map_or(Cow::Borrowed(serial), |(_, rule)| {
                    rule.replace(serial, "$code")
                }),
        }
    }
}
