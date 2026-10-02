use std::{collections::BTreeSet, error::Error, fs, path::PathBuf};

use rusqlite::{Connection, OpenFlags};
use shiratsu_naming::naming::{
    TokenizedName,
    nointro::{NoIntroName, NoIntroToken},
    tosec::{TOSECName, TOSECToken},
};

const QUERY: &str = r#"
SELECT DISTINCT game.platform_id, game.entry_name, game.naming_convention, serial.normalized
FROM game JOIN serial ON serial.game_id = game.game_id
WHERE length(serial.normalized) > 0
"#;

/// Writes the multidisc table read by `polymer-library` from a shiragame database.
///
/// Usage: `polymer-discsets <shiragame.db> <discsets.tsv>`
struct DiscSetGenerator {
    rows: BTreeSet<(String, String, u8, String)>,
}

impl DiscSetGenerator {
    fn read(database: &PathBuf) -> Result<Self, Box<dyn Error>> {
        let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut statement = connection.prepare(QUERY)?;
        let mut rows = statement.query(())?;
        let mut generator = Self {
            rows: BTreeSet::new(),
        };
        while let Some(row) = rows.next()? {
            let platform: String = row.get(0)?;
            let name: String = row.get(1)?;
            let convention: Option<String> = row.get(2)?;
            let serial: String = row.get(3)?;
            let parsed = match convention.as_deref() {
                Some("TOSEC") => Self::tosec(&name),
                _ => Self::nointro(&name),
            };
            if let Some((disc, set)) = parsed {
                generator.rows.insert((platform, serial, disc, set));
            }
        }
        Ok(generator)
    }

    fn disc(media_type: &str, number: &str) -> Option<u8> {
        let disc =
            media_type.eq_ignore_ascii_case("disc") || media_type.eq_ignore_ascii_case("disk");
        disc.then(|| number.parse().ok()).flatten()
    }

    fn nointro(name: &str) -> Option<(u8, String)> {
        let name = NoIntroName::try_parse(name).ok()?;
        let mut disc = None;
        let mut tokens = Vec::new();
        for token in name.iter() {
            match token {
                NoIntroToken::Media { media_type, number } if disc.is_none() => {
                    disc = Some(Self::disc(media_type, number)?);
                }
                token => tokens.push(token.clone()),
            }
        }
        Some((disc?, NoIntroName::from(tokens).to_string()))
    }

    fn tosec(name: &str) -> Option<(u8, String)> {
        let name = TOSECName::try_parse(name).ok()?;
        let mut disc = None;
        let mut tokens = Vec::new();
        for token in name.iter() {
            match token {
                TOSECToken::Media(parts) if disc.is_none() => {
                    let [part] = parts.as_slice() else {
                        return None;
                    };
                    disc = Some(Self::disc(part.media_type, part.number)?);
                }
                token => tokens.push(token.clone()),
            }
        }
        Some((disc?, TOSECName::from(tokens).to_string()))
    }

    fn write(&self, output: &PathBuf) -> std::io::Result<()> {
        let mut table = String::new();
        for (platform, serial, disc, set) in &self.rows {
            table.push_str(&format!("{platform}\t{serial}\t{disc}\t{set}\n"));
        }
        fs::write(output, table)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(database), Some(output)) = (arguments.next(), arguments.next()) else {
        return Err("usage: polymer-discsets <shiragame.db> <discsets.tsv>".into());
    };
    let generator = DiscSetGenerator::read(&database)?;
    generator.write(&output)?;
    println!(
        "wrote {} rows to {}",
        generator.rows.len(),
        output.display()
    );
    Ok(())
}
