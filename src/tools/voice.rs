use async_trait::async_trait;
use tracing::{debug, info, warn};

use super::{Tool, ToolContext, ToolOutput};
use crate::error::Result;

/// Voice transcription tool — transcribes audio to text using an
/// OpenAI-compatible Whisper endpoint (OpenAI, local whisper server, etc.).
pub struct TranscribeTool;

impl TranscribeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for TranscribeTool {
    fn name(&self) -> &str {
        "transcribe"
    }

    fn description(&self) -> &str {
        "Transcribe an audio file to text. Supports MP3, WAV, OGG, M4A, WEBM, FLAC. Uses Whisper via an OpenAI-compatible API."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["audio"],
            "properties": {
                "audio": {
                    "type": "string",
                    "description": "Path to audio file (sandbox-relative) or URL"
                },
                "language": {
                    "type": "string",
                    "description": "ISO 639-1 language code hint (e.g. 'en', 'es', 'de')"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let audio = params.get("audio").and_then(|v| v.as_str()).unwrap_or_default();
        let language = params.get("language").and_then(|v| v.as_str());

        if audio.is_empty() {
            return Ok(ToolOutput::error("audio file path or URL is required"));
        }

        let bytes = if audio.starts_with("http://") || audio.starts_with("https://") {
            match ctx.http_client.get(audio).send().await {
                Ok(resp) => match resp.bytes().await {
                    Ok(b) => b.to_vec(),
                    Err(e) => return Ok(ToolOutput::error(format!("download failed: {e}"))),
                },
                Err(e) => return Ok(ToolOutput::error(format!("fetch failed: {e}"))),
            }
        } else {
            match ctx.sandbox.read_binary(audio) {
                Ok(b) => b,
                Err(e) => return Ok(ToolOutput::error(format!("read failed: {e}"))),
            }
        };

        if bytes.len() > 25 * 1024 * 1024 {
            return Ok(ToolOutput::error("audio file too large (max 25 MiB)"));
        }

        let api_key = std::env::var("OPENAI_API_KEY")
            .or_else(|_| std::env::var("WHISPER_API_KEY"))
            .or_else(|_| std::env::var("OPENROUTER_API_KEY"))
            .unwrap_or_default();

        let base_url = std::env::var("WHISPER_API_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

        if api_key.is_empty() {
            return Ok(ToolOutput::error(
                "transcription requires OPENAI_API_KEY, WHISPER_API_KEY, or OPENROUTER_API_KEY"
            ));
        }

        let ext = audio.rsplit('.').next().unwrap_or("ogg");
        let filename = format!("audio.{ext}");
        let mime = match ext {
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "ogg" | "oga" => "audio/ogg",
            "m4a" => "audio/m4a",
            "webm" => "audio/webm",
            "flac" => "audio/flac",
            _ => "audio/ogg",
        };

        debug!(audio, size = bytes.len(), "sending to whisper API");

        let file_part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str(mime)
            .unwrap_or_else(|_| reqwest::multipart::Part::bytes(vec![]));

        let mut form = reqwest::multipart::Form::new()
            .text("model", "whisper-1")
            .part("file", file_part);

        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }

        let url = format!("{base_url}/audio/transcriptions");
        let resp = ctx
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                let text = body.get("text").and_then(|v| v.as_str()).unwrap_or_default();
                info!(chars = text.len(), "transcription complete");
                Ok(ToolOutput::ok(text.to_string()))
            }
            Ok(r) => {
                let status = r.status();
                let err = r.text().await.unwrap_or_default();
                warn!(status = %status, "whisper API error");
                Ok(ToolOutput::error(format!("Whisper API {status}: {err}")))
            }
            Err(e) => Ok(ToolOutput::error(format!("Whisper API request failed: {e}"))),
        }
    }
}

/// Text-to-speech tool — generates audio from text using an OpenAI-compatible TTS endpoint.
pub struct SpeakTool;

impl SpeakTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SpeakTool {
    fn name(&self) -> &str {
        "speak"
    }

    fn description(&self) -> &str {
        "Convert text to speech audio. Returns the path to the generated audio file. Uses an OpenAI-compatible TTS API."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to convert to speech"
                },
                "voice": {
                    "type": "string",
                    "description": "Voice name (default: 'alloy'). Options: alloy, echo, fable, onyx, nova, shimmer"
                },
                "output": {
                    "type": "string",
                    "description": "Output filename (default: 'speech.mp3')"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or_default();
        let voice = params.get("voice").and_then(|v| v.as_str()).unwrap_or("alloy");
        let output = params.get("output").and_then(|v| v.as_str()).unwrap_or("speech.mp3");

        if text.is_empty() {
            return Ok(ToolOutput::error("text is required"));
        }

        if text.len() > 4096 {
            return Ok(ToolOutput::error("text too long (max 4096 characters)"));
        }

        let api_key = std::env::var("OPENAI_API_KEY")
            .or_else(|_| std::env::var("TTS_API_KEY"))
            .unwrap_or_default();

        let base_url = std::env::var("TTS_API_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

        if api_key.is_empty() {
            return Ok(ToolOutput::error(
                "TTS requires OPENAI_API_KEY or TTS_API_KEY"
            ));
        }

        debug!(text_len = text.len(), voice, "generating speech");

        let body = serde_json::json!({
            "model": "tts-1",
            "input": text,
            "voice": voice,
            "response_format": "mp3"
        });

        let url = format!("{base_url}/audio/speech");
        let resp = ctx
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let audio_bytes = r.bytes().await.unwrap_or_default();
                match ctx.sandbox.write(std::path::Path::new(output), &audio_bytes) {
                    Ok(()) => {
                        info!(output, bytes = audio_bytes.len(), "TTS audio saved");
                        Ok(ToolOutput::ok(format!("Audio saved to {output} ({} bytes)", audio_bytes.len())))
                    }
                    Err(e) => Ok(ToolOutput::error(format!("failed to save audio: {e}"))),
                }
            }
            Ok(r) => {
                let status = r.status();
                let err = r.text().await.unwrap_or_default();
                warn!(status = %status, "TTS API error");
                Ok(ToolOutput::error(format!("TTS API {status}: {err}")))
            }
            Err(e) => Ok(ToolOutput::error(format!("TTS API request failed: {e}"))),
        }
    }
}
