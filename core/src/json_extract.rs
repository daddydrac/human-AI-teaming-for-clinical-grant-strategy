use anyhow::{bail, Result};
use serde::de::DeserializeOwned;

pub fn parse_json_from_model<T: DeserializeOwned>(text: &str) -> Result<T> {
    if let Ok(v) = serde_json::from_str::<T>(text) { return Ok(v); }
    let trimmed = text.trim();
    let unfenced = trimmed
        .strip_prefix("```json").or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    if let Ok(v) = serde_json::from_str::<T>(unfenced) { return Ok(v); }

    let bytes = unfenced.as_bytes();
    for (open, close) in [(b'{', b'}'), (b'[', b']')] {
        if let Some(start) = bytes.iter().position(|b| *b == open) {
            let mut depth = 0i32;
            let mut in_string = false;
            let mut escape = false;
            for (i, b) in bytes.iter().enumerate().skip(start) {
                if in_string {
                    if escape { escape = false; continue; }
                    if *b == b'\\' { escape = true; continue; }
                    if *b == b'"' { in_string = false; }
                    continue;
                }
                if *b == b'"' { in_string = true; continue; }
                if *b == open { depth += 1; }
                if *b == close { depth -= 1; }
                if depth == 0 {
                    let slice = &unfenced[start..=i];
                    if let Ok(v) = serde_json::from_str::<T>(slice) { return Ok(v); }
                    break;
                }
            }
        }
    }
    bail!("model response did not contain valid JSON")
}
