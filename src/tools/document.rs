use async_trait::async_trait;
use tracing::{debug, warn};

use super::{Tool, ToolContext, ToolOutput};
use crate::error::Result;

/// Document understanding tool — extracts text from PDFs, DOCX, XLSX, and plain text files.
pub struct DocumentTool;

impl DocumentTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for DocumentTool {
    fn name(&self) -> &str {
        "document"
    }

    fn description(&self) -> &str {
        "Extract text and structure from documents. Supports PDF, DOCX, XLSX, CSV, and plain text files. Provide a file path (sandbox-relative) or URL."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["file"],
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Path to document (sandbox-relative) or URL"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to extract (default: 50000)"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let file = params.get("file").and_then(|v| v.as_str()).unwrap_or_default();
        let max_chars = params.get("max_chars").and_then(|v| v.as_u64()).unwrap_or(50_000) as usize;

        if file.is_empty() {
            return Ok(ToolOutput::error("file path or URL is required"));
        }

        let bytes = if file.starts_with("http://") || file.starts_with("https://") {
            match ctx.http_client.get(file).send().await {
                Ok(resp) => match resp.bytes().await {
                    Ok(b) => b.to_vec(),
                    Err(e) => return Ok(ToolOutput::error(format!("download failed: {e}"))),
                },
                Err(e) => return Ok(ToolOutput::error(format!("fetch failed: {e}"))),
            }
        } else {
            match ctx.sandbox.read_binary(file) {
                Ok(b) => b,
                Err(e) => return Ok(ToolOutput::error(format!("read failed: {e}"))),
            }
        };

        let ext = file.rsplit('.').next().map(|s| s.to_lowercase()).unwrap_or_default();
        debug!(file, ext = %ext, size = bytes.len(), "extracting document");

        let text = match ext.as_str() {
            "pdf" => extract_pdf(&bytes),
            "docx" => extract_docx(&bytes),
            "xlsx" | "xls" => extract_xlsx(&bytes),
            "csv" => extract_csv(&bytes),
            "txt" | "md" | "rst" | "json" | "toml" | "yaml" | "yml" | "xml" | "html" | "htm" => {
                String::from_utf8_lossy(&bytes).to_string()
            }
            _ => {
                // Try as UTF-8 text, fallback to error
                match String::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(_) => return Ok(ToolOutput::error(format!(
                        "unsupported document format: .{ext}"
                    ))),
                }
            }
        };

        if text.is_empty() {
            return Ok(ToolOutput::error("no text could be extracted from the document"));
        }

        let truncated = if text.len() > max_chars {
            format!("{}...\n\n[truncated at {} chars, total {}]", &text[..max_chars], max_chars, text.len())
        } else {
            text
        };

        Ok(ToolOutput::ok(truncated))
    }
}

fn extract_pdf(bytes: &[u8]) -> String {
    // Simple PDF text extraction using the built-in approach:
    // Look for text streams between BT/ET markers and extract Tj/TJ strings.
    // This handles basic PDFs; complex ones may need a full parser.
    let content = String::from_utf8_lossy(bytes);
    let mut result = String::new();

    // Extract text between parentheses in Tj/TJ operators
    let mut in_text = false;
    let mut paren_depth = 0;
    let mut current_text = String::new();

    for ch in content.chars() {
        if in_text {
            match ch {
                '(' => {
                    paren_depth += 1;
                    if paren_depth > 1 {
                        current_text.push(ch);
                    }
                }
                ')' => {
                    paren_depth -= 1;
                    if paren_depth == 0 {
                        in_text = false;
                        if !current_text.is_empty() {
                            result.push_str(&current_text);
                            result.push(' ');
                            current_text.clear();
                        }
                    } else {
                        current_text.push(ch);
                    }
                }
                _ => {
                    current_text.push(ch);
                }
            }
        } else if ch == '(' {
            in_text = true;
            paren_depth = 1;
        }
    }

    // Clean up: replace escaped chars, collapse whitespace
    let result = result
        .replace("\\n", "\n")
        .replace("\\r", "")
        .replace("\\t", "\t")
        .replace("\\(", "(")
        .replace("\\)", ")");

    // Collapse multiple spaces
    let mut clean = String::with_capacity(result.len());
    let mut prev_space = false;
    for ch in result.chars() {
        if ch == ' ' || ch == '\t' {
            if !prev_space {
                clean.push(' ');
            }
            prev_space = true;
        } else {
            prev_space = false;
            clean.push(ch);
        }
    }

    clean.trim().to_string()
}

fn extract_docx(bytes: &[u8]) -> String {
    // DOCX is a ZIP containing word/document.xml — extract text from XML
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = match zip::ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(e) => {
            warn!(err = %e, "failed to open DOCX as ZIP");
            return String::new();
        }
    };

    let mut text = String::new();
    if let Ok(mut file) = archive.by_name("word/document.xml") {
        let mut xml = String::new();
        if std::io::Read::read_to_string(&mut file, &mut xml).is_ok() {
            // Strip XML tags, keeping text content
            text = strip_xml_tags(&xml);
        }
    }
    text
}

fn extract_xlsx(bytes: &[u8]) -> String {
    // XLSX is a ZIP; shared strings are in xl/sharedStrings.xml,
    // actual data in xl/worksheets/sheet*.xml. Extract shared strings.
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = match zip::ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(e) => {
            warn!(err = %e, "failed to open XLSX as ZIP");
            return String::new();
        }
    };

    let mut text = String::new();

    // Extract shared strings
    if let Ok(mut file) = archive.by_name("xl/sharedStrings.xml") {
        let mut xml = String::new();
        if std::io::Read::read_to_string(&mut file, &mut xml).is_ok() {
            text.push_str(&strip_xml_tags(&xml));
            text.push('\n');
        }
    }

    // Extract worksheet data
    for i in 1..=10 {
        let name = format!("xl/worksheets/sheet{i}.xml");
        if let Ok(mut file) = archive.by_name(&name) {
            let mut xml = String::new();
            if std::io::Read::read_to_string(&mut file, &mut xml).is_ok() {
                text.push_str(&format!("--- Sheet {i} ---\n"));
                text.push_str(&strip_xml_tags(&xml));
                text.push('\n');
            }
        }
    }

    text
}

fn extract_csv(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn strip_xml_tags(xml: &str) -> String {
    let mut result = String::with_capacity(xml.len());
    let mut in_tag = false;
    for ch in xml.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                result.push(' ');
            }
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    // Collapse whitespace
    let mut clean = String::with_capacity(result.len());
    let mut prev_space = false;
    for ch in result.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                clean.push(' ');
            }
            prev_space = true;
        } else {
            prev_space = false;
            clean.push(ch);
        }
    }
    clean.trim().to_string()
}
