//! Minimal INI parser preserving section order and key order.
//!
//! The legacy MangaMeeyaCE.ini format is simple:
//! - Sections look like `[Name]`.
//! - Entries are `Key=Value` (values may contain commas; unquoted).
//! - Lines starting with `;` are comments.
//! - Empty lines separate sections visually.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Default)]
pub struct Ini {
    sections: Vec<Section>,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    entries: Vec<(String, String)>,
    index: BTreeMap<String, usize>,
}

impl Section {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            entries: Vec::new(),
            index: BTreeMap::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.index
            .get(&key.to_ascii_lowercase())
            .map(|&i| self.entries[i].1.as_str())
    }

    pub fn get_parse<T: FromStr>(&self, key: &str) -> Option<T> {
        self.get(key).and_then(|v| v.trim().parse().ok())
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(|v| match v.trim() {
            "0" | "false" | "False" | "FALSE" => Some(false),
            "1" | "true" | "True" | "TRUE" => Some(true),
            _ => None,
        })
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let lc = key.to_ascii_lowercase();
        if let Some(&i) = self.index.get(&lc) {
            self.entries[i].1 = value.into();
        } else {
            self.index.insert(lc, self.entries.len());
            self.entries.push((key.to_string(), value.into()));
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

impl Ini {
    pub fn parse(text: &str) -> Self {
        let mut ini = Ini::default();
        let mut current: Option<Section> = None;
        for raw in text.lines() {
            let line = raw.trim_start_matches('\u{FEFF}').trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                if let Some(sec) = current.take() {
                    ini.push_section(sec);
                }
                current = Some(Section::new(rest.trim()));
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let sec = current.get_or_insert_with(|| Section::new(""));
                sec.set(k.trim(), v.trim());
            }
        }
        if let Some(sec) = current.take() {
            ini.push_section(sec);
        }
        ini
    }

    fn push_section(&mut self, sec: Section) {
        self.sections.push(sec);
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
    }

    pub fn section_mut(&mut self, name: &str) -> &mut Section {
        if let Some(idx) = self
            .sections
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
        {
            &mut self.sections[idx]
        } else {
            self.sections.push(Section::new(name));
            self.sections.last_mut().unwrap()
        }
    }

    pub fn take_section(&mut self, name: &str) -> Option<Section> {
        let idx = self
            .sections
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))?;
        Some(self.sections.remove(idx))
    }

    pub fn sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter()
    }
}

impl fmt::Display for Ini {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, sec) in self.sections.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            if !sec.name.is_empty() {
                writeln!(f, "[{}]", sec.name)?;
            }
            for (k, v) in sec.entries() {
                writeln!(f, "{}={}", k, v)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple() {
        let ini = Ini::parse("[A]\nx=1\ny=hi\n\n[B]\nz=2\n");
        assert_eq!(ini.section("A").unwrap().get("x"), Some("1"));
        assert_eq!(ini.section("B").unwrap().get("z"), Some("2"));
    }

    #[test]
    fn roundtrip_preserves_order() {
        let text = "[A]\nx=1\ny=2\n\n[B]\nz=3\n";
        let ini = Ini::parse(text);
        assert_eq!(ini.to_string().trim(), text.trim());
    }
}
