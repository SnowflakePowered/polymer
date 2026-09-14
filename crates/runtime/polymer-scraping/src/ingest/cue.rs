use crate::ScrapingError;
use polymer_library::{CuePart, CueTemplate, PackageManifest};

pub(crate) struct CuePreparation;

impl CuePreparation {
    pub(crate) fn finalize(manifest: &mut PackageManifest) -> Result<(), ScrapingError> {
        Self::validate_files(manifest)?;
        let sheet = cuna::CueSheet::new(&manifest.cue)
            .map_err(|error| ScrapingError::InvalidMedia(error.to_string()))?;
        let references: Vec<_> = sheet
            .files
            .iter()
            .map(|file| file.name.as_str())
            .chain(sheet.header.cdtextfile.as_deref())
            .collect();
        for name in references {
            if !manifest
                .files
                .iter()
                .any(|file| file.name.replace('\\', "/") == name.replace('\\', "/"))
            {
                return Err(ScrapingError::InvalidMedia(format!(
                    "CUE references an unbound file: {name}"
                )));
            }
        }
        let mut parts = Vec::new();
        let mut count = 0;
        for line in manifest.cue.split_inclusive('\n') {
            let Some((start, end)) = Self::operand(line)? else {
                parts.push(CuePart::Text(line.to_owned()));
                continue;
            };
            let name = line[start..end].trim_matches('"');
            let index = manifest
                .files
                .iter()
                .position(|file| file.name.replace('\\', "/") == name.replace('\\', "/"))
                .ok_or_else(|| {
                    ScrapingError::InvalidMedia(format!("unbound CUE operand: {name}"))
                })?;
            parts.push(CuePart::Text(line[..start].to_owned()));
            parts.push(CuePart::File(index));
            parts.push(CuePart::Text(line[end..].to_owned()));
            count += 1;
        }
        if count != sheet.files.len() + usize::from(sheet.header.cdtextfile.is_some()) {
            return Err(ScrapingError::InvalidMedia(
                "CUE operand count differs from parsed descriptor".into(),
            ));
        }
        manifest.template = Some(CueTemplate { parts });
        manifest.error = None;
        Ok(())
    }

    /// cuna validates syntax; retain all original text except FILE/CDTEXTFILE operands.
    fn operand(line: &str) -> Result<Option<(usize, usize)>, ScrapingError> {
        let text = line.trim_start_matches('\u{feff}').trim_start();
        let command_end = text.find(char::is_whitespace).unwrap_or(text.len());
        let command = &text[..command_end];
        if !command.eq_ignore_ascii_case("FILE") && !command.eq_ignore_ascii_case("CDTEXTFILE") {
            return Ok(None);
        }
        let operand = text[command_end..].trim_start();
        let start = line.len() - operand.len();
        let len = if command.eq_ignore_ascii_case("CDTEXTFILE") {
            Some(operand.trim_end().len())
        } else if let Some(quoted) = operand.strip_prefix('"') {
            quoted.find('"').map(|end| end + 2)
        } else {
            Some(operand.find(char::is_whitespace).unwrap_or(operand.len()))
        };
        let len = len
            .filter(|len| *len > 0)
            .ok_or_else(|| ScrapingError::InvalidMedia("invalid CUE file operand".into()))?;
        Ok(Some((start, start + len)))
    }
}
