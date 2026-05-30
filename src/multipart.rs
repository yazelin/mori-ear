//! 極簡 multipart/form-data 解析 — 只取我們要的:`file`(WAV bytes)+ 幾個文字欄位
//! (language / prompt / backend / cleanup)。給 mori-ear HTTP 服務的 `POST /inference` 用。
//!
//! 為什麼自己解析:要對齊 whisper-server-contract 的 multipart 形狀(AgentOS whisper_client
//! 與 mori-desktop 都用 reqwest 送 multipart `file`),但 ear 的服務走 tiny_http(blocking、
//! 無內建 multipart 解析),又要守住極簡依賴(不引 multipart crate)。multipart/form-data 是
//! 穩定標準、consumer 受控(我們自家),自解可靠且可單元測試。
//!
//! 邊界處理鐵則:RFC 2046 — 每個 boundary 前的 CRLF 屬於 delimiter。本解析以 `--<boundary>`
//! 為切點,逐段去頭尾 CRLF、再以第一個 `\r\n\r\n` 切 headers/data,保留 data 內含的任何 bytes
//! (含內嵌 CRLF / binary WAV)。

/// 一個 form part。
pub struct Field {
    pub name: String,
    /// 解析自 Content-Disposition 的 `filename="..."`。目前轉錄不需用到(只看 `name`/`data`),
    /// 但完整解出來便於除錯 / 日後擴充,故保留。
    #[allow(dead_code)]
    pub filename: Option<String>,
    pub data: Vec<u8>,
}

/// 解析後的整份 form。
pub struct Form {
    pub fields: Vec<Field>,
}

impl Form {
    /// 取 `file` part 的原始 bytes(WAV)。
    pub fn file(&self) -> Option<&[u8]> {
        self.fields
            .iter()
            .find(|f| f.name == "file")
            .map(|f| f.data.as_slice())
    }

    /// 取某個文字欄位(trim 後)。
    pub fn text(&self, name: &str) -> Option<String> {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| String::from_utf8_lossy(&f.data).trim().to_string())
    }
}

/// 從 Content-Type 取 boundary。`multipart/form-data; boundary=xxx`(可帶引號 / 後接其他參數)。
pub fn boundary_of(content_type: &str) -> Option<String> {
    let lower = content_type.to_ascii_lowercase();
    let idx = lower.find("boundary=")?;
    let rest = &content_type[idx + "boundary=".len()..];
    let raw = rest.split(';').next().unwrap_or(rest).trim();
    let raw = raw.trim_matches('"');
    (!raw.is_empty()).then(|| raw.to_string())
}

fn find_from(hay: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if needle.is_empty() || start >= hay.len() || needle.len() > hay.len() - start {
        return None;
    }
    hay[start..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + start)
}

/// 解析 body → 各 part。容忍前綴 preamble 與結尾 `--<boundary>--`。
pub fn parse(boundary: &str, body: &[u8]) -> Form {
    let dash = format!("--{boundary}");
    let dash = dash.as_bytes();
    let mut fields = Vec::new();

    // 收集所有 `--<boundary>` 起點(含結尾的 closing,closing 後接 "--")。
    let mut bounds = Vec::new();
    let mut i = 0;
    while let Some(p) = find_from(body, dash, i) {
        bounds.push(p);
        i = p + dash.len();
    }

    // 相鄰兩個 boundary 之間就是一個 part 的內容(連同首尾 CRLF)。
    for w in bounds.windows(2) {
        let seg_start = w[0] + dash.len();
        let seg_end = w[1];
        if seg_start >= seg_end {
            continue;
        }
        let mut seg = &body[seg_start..seg_end];
        // 若這段以 "--" 起頭,代表 w[0] 是 closing boundary,不是 part。
        if seg.starts_with(b"--") {
            continue;
        }
        // 去頭 CRLF(boundary 與 part headers 間的 CRLF)。
        seg = seg.strip_prefix(b"\r\n").unwrap_or(seg);
        // 去尾 CRLF(下一個 boundary 前的 CRLF 屬於 delimiter)。
        if seg.ends_with(b"\r\n") {
            seg = &seg[..seg.len() - 2];
        }
        // 以第一個空行切 headers / data。
        let Some(hpos) = find_from(seg, b"\r\n\r\n", 0) else {
            continue;
        };
        let headers = &seg[..hpos];
        let data = &seg[hpos + 4..];
        let (name, filename) = parse_disposition(headers);
        if let Some(name) = name {
            fields.push(Field {
                name,
                filename,
                data: data.to_vec(),
            });
        }
    }
    Form { fields }
}

fn parse_disposition(headers: &[u8]) -> (Option<String>, Option<String>) {
    let text = String::from_utf8_lossy(headers);
    for line in text.split("\r\n") {
        let l = line.trim();
        if l.to_ascii_lowercase().starts_with("content-disposition:") {
            return (extract_param(l, "name"), extract_param(l, "filename"));
        }
    }
    (None, None)
}

/// 從一行裡抓 `key="value"`。
fn extract_param(line: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let idx = line.find(&pat)?;
    let rest = &line[idx + pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 組一份標準 multipart body:file(可含 binary / 內嵌 CRLF)+ 一個文字欄。
    fn build(boundary: &str, file: &[u8], lang: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        b.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
        );
        b.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
        b.extend_from_slice(file);
        b.extend_from_slice(b"\r\n");
        b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        b.extend_from_slice(b"Content-Disposition: form-data; name=\"language\"\r\n\r\n");
        b.extend_from_slice(lang.as_bytes());
        b.extend_from_slice(b"\r\n");
        b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        b
    }

    #[test]
    fn boundary_extract() {
        assert_eq!(
            boundary_of("multipart/form-data; boundary=abc123").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            boundary_of("multipart/form-data; boundary=\"x y\"; charset=utf8").as_deref(),
            Some("x y")
        );
        assert_eq!(boundary_of("application/json"), None);
    }

    #[test]
    fn parse_file_and_text_preserves_binary() {
        // file payload 故意含內嵌 CRLF + 偽 WAV magic → 確認 binary 原封不動。
        let file = b"RIFF\x00\x01WAVEdata\r\nmid-crlf\x00\xff";
        let body = build("----BoundaryXYZ", file, "zh");
        let form = parse("----BoundaryXYZ", &body);
        assert_eq!(form.file().unwrap(), file, "file bytes（含內嵌 CRLF/binary）須原封保留");
        assert_eq!(form.text("language").as_deref(), Some("zh"));
        let file_field = form.fields.iter().find(|f| f.name == "file").unwrap();
        assert_eq!(file_field.filename.as_deref(), Some("audio.wav"));
    }

    #[test]
    fn missing_field_is_none() {
        let body = build("b", b"hello-wav", "en");
        let form = parse("b", &body);
        assert!(form.file().is_some());
        assert_eq!(form.text("language").as_deref(), Some("en"));
        assert!(form.text("does-not-exist").is_none());
    }

    #[test]
    fn empty_file_part_round_trips() {
        let body = build("z", b"", "auto");
        let form = parse("z", &body);
        assert_eq!(form.file(), Some(&b""[..]), "空 file part 應為空 slice 而非缺欄");
    }
}
