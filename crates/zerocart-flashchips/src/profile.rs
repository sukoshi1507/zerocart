//! Profile files. They are almost JSON, but numbers may be written as 0x.. hex: converted before parsing.
use serde_json::Value;
use std::path::Path;

pub type Cmds = Vec<(u32, u16)>;

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub file: String,
    /// "DMG" or "AGB"
    pub kind: String,
    pub name: String,
    /// Plain flash ID byte sequences. Sets that use the grouped (per-bank) form are not kept.
    pub flash_ids: Vec<Vec<u8>>,
    pub flash_id_offsets: Option<Vec<usize>>,
    pub voltage: f64,
    pub flash_size: u64,
    pub command_set: Option<String>,
    pub manual_select: bool,
    pub write_pin: Option<String>,
    pub read_identifier_at: u32,
    pub reset: Cmds,
    pub unlock: Cmds,
    pub read_identifier: Cmds,
    pub read_cfi: Option<Cmds>,
    /// Keys that mark profiles with their own detection routine in FlashGBX (not handled by the generic probe).
    pub special: bool,
    pub raw: Value,
}

/// Replaces hex literals outside strings by decimal ones.
pub fn hex_to_json(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let (mut out, mut i, mut in_str) = (String::with_capacity(text.len()), 0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 1;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == '0' && i + 1 < b.len() && (b[i + 1] == 'x' || b[i + 1] == 'X') {
            let mut j = i + 2;
            let mut v: u64 = 0;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                v = v * 16 + b[j].to_digit(16).unwrap() as u64;
                j += 1;
            }
            out.push_str(&v.to_string());
            i = j;
            continue;
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

fn cmds(v: Option<&Value>) -> Option<Cmds> {
    let mut out = Vec::new();
    for item in v?.as_array()? {
        let a = item.as_array()?;
        out.push((a.first()?.as_u64()? as u32, a.get(1)?.as_u64()? as u16));
    }
    Some(out)
}

pub fn parse(file: &str, text: &str) -> Result<Profile, String> {
    let v: Value = serde_json::from_str(&hex_to_json(text)).map_err(|e| format!("{file}: {e}"))?;
    let commands = v.get("commands");
    let get = |k: &str| commands.and_then(|c| c.get(k));
    let flash_ids = v
        .get("flash_ids")
        .and_then(|f| f.as_array())
        .map(|sets| {
            sets.iter()
                .filter_map(|s| {
                    let a = s.as_array()?;
                    if a.get(1).is_some_and(|x| x.is_array()) {
                        return None; // grouped form
                    }
                    a.iter().map(|x| x.as_u64().map(|n| n as u8)).collect::<Option<Vec<u8>>>()
                })
                .collect()
        })
        .unwrap_or_default();
    let command_set = v.get("command_set").and_then(|x| x.as_str()).map(String::from);
    let special = matches!(
        command_set.as_deref(),
        Some("BLAZE_XPLODER" | "DATEL_ORBITV2" | "GBMEMORY" | "GBAMP" | "BUNG_16M")
    ) || v.get("m29w640").is_some()
        || v.get("dmg-mbc5-32m-flash").is_some();
    Ok(Profile {
        file: file.to_string(),
        kind: v.get("type").and_then(|x| x.as_str()).unwrap_or("").to_uppercase(),
        name: v.get("names").and_then(|n| n.get(0)).and_then(|n| n.as_str()).unwrap_or(file).to_string(),
        flash_ids,
        flash_id_offsets: v
            .get("flash_id_offsets")
            .and_then(|o| o.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as usize)).collect()),
        voltage: v.get("voltage").and_then(|x| x.as_f64()).unwrap_or(0.0),
        flash_size: v.get("flash_size").and_then(|x| x.as_u64()).unwrap_or(0),
        command_set,
        manual_select: v.get("manual_select").and_then(|x| x.as_bool()).unwrap_or(false),
        write_pin: v.get("write_pin").and_then(|x| x.as_str()).map(String::from),
        read_identifier_at: v.get("read_identifier_at").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        reset: cmds(get("reset")).unwrap_or_default(),
        unlock: cmds(get("unlock")).unwrap_or_default(),
        read_identifier: cmds(get("read_identifier")).unwrap_or_default(),
        read_cfi: cmds(get("read_cfi")),
        special,
        raw: v,
    })
}

/// Loads every `fc_*.txt` of a directory, sorted by file name. Unreadable files are returned as errors.
pub fn load_dir(dir: &Path) -> (Vec<Profile>, Vec<String>) {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("fc_") && n.ends_with(".txt")));
    files.sort();
    let (mut ok, mut errors) = (Vec::new(), Vec::new());
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| parse(&name, &t)) {
            Ok(p) => ok.push(p),
            Err(e) => errors.push(e),
        }
    }
    (ok, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_converted_outside_strings_only() {
        assert_eq!(hex_to_json(r#"[ 0xAAA, 0x55, "0x10 stays" ]"#), r#"[ 2730, 85, "0x10 stays" ]"#);
    }

    #[test]
    fn parses_a_profile() {
        let p = parse("fc_test.txt", r#"{"type":"DMG","names":["T"],"flash_ids":[[0x01,0xAD]],"voltage":3.3,
            "commands":{"reset":[[0,0xF0]],"read_identifier":[[0xAAA,0xAA],[0x555,0x55],[0xAAA,0x90]]}}"#).unwrap();
        assert_eq!((p.kind.as_str(), p.name.as_str()), ("DMG", "T"));
        assert_eq!(p.flash_ids, vec![vec![1, 0xAD]]);
        assert_eq!(p.read_identifier, vec![(0xAAA, 0xAA), (0x555, 0x55), (0xAAA, 0x90)]);
        assert_eq!(p.reset, vec![(0, 0xF0)]);
    }

    #[test]
    fn all_bundled_profiles_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs");
        let (ok, errors) = load_dir(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(ok.len() >= 160, "only {} profiles", ok.len());
        assert!(ok.iter().any(|p| p.kind == "AGB") && ok.iter().any(|p| p.kind == "DMG"));
    }
}
