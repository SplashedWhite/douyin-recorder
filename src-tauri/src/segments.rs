use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentOutput {
    pub file_prefix: String,
    pub output_pattern: String,
    pub manifest_path: String,
    pub duration_secs: u64,
}

impl SegmentOutput {
    pub fn new(directory: &Path, name: &str, duration_secs: u64) -> Self {
        let file_prefix = directory.join(name).to_string_lossy().into_owned();
        Self {
            output_pattern: format!("{}_part%03d.flv", file_prefix.replace('%', "%%")),
            manifest_path: directory
                .join(".recording-meta")
                .join(format!("{name}.csv"))
                .to_string_lossy()
                .into_owned(),
            file_prefix,
            duration_secs,
        }
    }

    pub fn path(&self, index: u32) -> String {
        format!("{}_part{index:03}.flv", self.file_prefix)
    }

    pub fn index_of(&self, filename: &str) -> Option<u32> {
        let prefix = Path::new(&self.file_prefix).file_name()?.to_str()?;
        let suffix = filename.strip_prefix(prefix)?.strip_prefix("_part")?;
        let digits = suffix
            .strip_suffix(".flv")
            .or_else(|| suffix.strip_suffix(".mp4"))?;
        if digits.len() < 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok().filter(|i| *i > 0)
    }

    pub fn next_exists(&self, index: u32) -> bool {
        index.checked_add(1).is_some_and(|i| {
            let path = PathBuf::from(self.path(i));
            path.exists() || path.with_extension("mp4").exists()
        })
    }

    pub fn files(&self) -> Result<BTreeMap<u32, PathBuf>, String> {
        let directory = Path::new(&self.file_prefix)
            .parent()
            .ok_or("分段目录无效")?;
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(directory).map_err(|e| format!("读取分段目录失败: {e}"))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                continue;
            }
            if let Some(index) = self.index_of(&entry.file_name().to_string_lossy()) {
                // A surviving FLV is authoritative until the conversion is committed.
                if entry.path().extension().is_some_and(|ext| ext == "flv")
                    || !files.contains_key(&index)
                {
                    files.insert(index, entry.path());
                }
            }
        }
        Ok(files)
    }
}

pub fn safe_filename(value: &str) -> String {
    let name: String = value
        .chars()
        .map(|ch| {
            if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
                '_'
            } else {
                ch
            }
        })
        .take(80)
        .collect();
    let name = name.trim_matches([' ', '.']);
    if name.is_empty() {
        "主播".into()
    } else {
        name.into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordSegment {
    pub id: i64,
    pub task_id: i64,
    pub segment_index: u32,
    pub file_path: String,
    pub file_size: i64,
    pub start_time: String,
    pub end_time: Option<String>,
    pub status: String,
    pub conversion_state: String,
    pub conversion_error: Option<String>,
    pub deleted: bool,
}

#[derive(Debug, Clone)]
pub struct ManifestEntry {
    pub index: u32,
    pub start: f64,
    pub end: f64,
}

#[derive(Default)]
pub struct ManifestReader {
    offset: u64,
    pending: Vec<u8>,
}

impl ManifestReader {
    pub fn read_all(output: &SegmentOutput) -> Result<Vec<ManifestEntry>, String> {
        let mut reader = Self::default();
        let mut entries = vec![];
        loop {
            let batch = reader.read(output)?;
            if batch.is_empty() {
                return Ok(entries);
            }
            entries.extend(batch);
        }
    }

    pub fn read(&mut self, output: &SegmentOutput) -> Result<Vec<ManifestEntry>, String> {
        let mut file = match std::fs::File::open(&output.manifest_path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(format!("读取分段清单失败: {e}")),
        };
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|e| e.to_string())?;
        let count = file
            .take(1024 * 1024)
            .read_to_end(&mut self.pending)
            .map_err(|e| e.to_string())?;
        self.offset += count as u64;
        let Some(end) = self.pending.iter().rposition(|b| *b == b'\n') else {
            return Ok(vec![]);
        };
        let complete: Vec<u8> = self.pending.drain(..=end).collect();
        let text = std::str::from_utf8(&complete).map_err(|e| format!("分段清单编码无效: {e}"))?;
        text.lines()
            .filter(|line| !line.is_empty())
            .map(|line| parse_entry(line, output))
            .collect()
    }
}

// FFmpeg emits RFC 4180 quoting. Names are sanitized to exclude line breaks.
fn parse_entry(line: &str, output: &SegmentOutput) -> Result<ManifestEntry, String> {
    let mut fields = vec![];
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.trim_end_matches('\r').chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => {
                fields.push(std::mem::take(&mut field));
            }
            _ => field.push(ch),
        }
    }
    fields.push(field);
    if quoted || fields.len() != 3 {
        return Err("分段清单格式无效".into());
    }
    let index = output
        .index_of(&fields[0])
        .ok_or("分段清单包含不属于本场的文件")?;
    let start: f64 = fields[1].parse().map_err(|_| "分段开始时间无效")?;
    let end: f64 = fields[2].parse().map_err(|_| "分段结束时间无效")?;
    if !start.is_finite() || !end.is_finite() || end < start {
        return Err("分段时间无效".into());
    }
    Ok(ManifestEntry { index, start, end })
}

pub fn segment_time(start_time: &str, offset: f64) -> String {
    let parsed = chrono::NaiveDateTime::parse_from_str(start_time, "%Y-%m-%d %H:%M:%S")
        .map(|time| time.and_utc())
        .unwrap_or_else(|_| chrono::Utc::now());
    parsed
        .checked_add_signed(chrono::Duration::milliseconds((offset * 1000.0) as i64))
        .unwrap_or(parsed)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_incremental_csv_with_unicode_commas_percent_and_partial_writes() {
        let dir = tempfile::tempdir().unwrap();
        let output = SegmentOutput::new(dir.path(), "主播, 100%_123_001", 60);
        std::fs::create_dir_all(Path::new(&output.manifest_path).parent().unwrap()).unwrap();
        assert!(output.output_pattern.contains("100%%"));
        assert!(output.path(1000).ends_with("part1000.flv"));
        let name = PathBuf::from(output.path(1))
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let line = format!("\"{name}\",0.000000,60.123000\n");
        let mut file = std::fs::File::create(&output.manifest_path).unwrap();
        let mut reader = ManifestReader::default();
        file.write_all(&line.as_bytes()[..4]).unwrap();
        assert!(reader.read(&output).unwrap().is_empty());
        file.write_all(&line.as_bytes()[4..line.len() - 1]).unwrap();
        assert!(reader.read(&output).unwrap().is_empty());
        file.write_all(b"\n").unwrap();
        let entries = reader.read(&output).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].index, 1);
        assert_eq!(entries[0].end, 60.123);
        assert!(reader.read(&output).unwrap().is_empty());
        assert!(output.index_of("unrelated_part001.flv").is_none());
        assert!(output
            .index_of(&name.replace("001.flv", "001.tmp.mp4"))
            .is_none());
        assert_eq!(safe_filename("../坏:名/称\\?\n"), "_坏_名_称___");
    }
}
