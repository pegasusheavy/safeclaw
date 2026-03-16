use serde::{Deserialize, Serialize};

/// Rich content that platforms with native support render natively,
/// and others fall back to a text representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RichContent {
    Image {
        url: String,
        #[serde(default)]
        caption: Option<String>,
    },
    File {
        url: String,
        filename: String,
        #[serde(default)]
        caption: Option<String>,
    },
    Buttons {
        text: String,
        buttons: Vec<InlineButton>,
    },
    Card {
        title: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        image_url: Option<String>,
        #[serde(default)]
        url: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineButton {
    pub label: String,
    pub data: String,
    #[serde(default)]
    pub style: ButtonStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ButtonStyle {
    #[default]
    Link,
    Primary,
    Secondary,
}

impl RichContent {
    /// Plain-text fallback for backends that don't support rich messages.
    pub fn to_text_fallback(&self) -> String {
        match self {
            RichContent::Image { url, caption } => match caption {
                Some(c) => format!("{c}\n{url}"),
                None => url.clone(),
            },
            RichContent::File {
                url,
                filename,
                caption,
            } => match caption {
                Some(c) => format!("{c}\n[{filename}]({url})"),
                None => format!("[{filename}]({url})"),
            },
            RichContent::Buttons { text, buttons } => {
                let labels: Vec<_> = buttons.iter().map(|b| b.label.as_str()).collect();
                format!("{text}\n\nOptions: {}", labels.join(" | "))
            }
            RichContent::Card {
                title,
                description,
                image_url,
                url,
            } => {
                let mut parts = vec![format!("**{title}**")];
                if let Some(d) = description {
                    parts.push(d.clone());
                }
                if let Some(img) = image_url {
                    parts.push(img.clone());
                }
                if let Some(u) = url {
                    parts.push(u.clone());
                }
                parts.join("\n")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_text_fallback_with_caption() {
        let rich = RichContent::Image {
            url: "https://example.com/img.png".into(),
            caption: Some("A photo".into()),
        };
        assert_eq!(
            rich.to_text_fallback(),
            "A photo\nhttps://example.com/img.png"
        );
    }

    #[test]
    fn image_text_fallback_without_caption() {
        let rich = RichContent::Image {
            url: "https://example.com/img.png".into(),
            caption: None,
        };
        assert_eq!(rich.to_text_fallback(), "https://example.com/img.png");
    }

    #[test]
    fn buttons_text_fallback() {
        let rich = RichContent::Buttons {
            text: "Choose one".into(),
            buttons: vec![
                InlineButton {
                    label: "Yes".into(),
                    data: "yes".into(),
                    style: ButtonStyle::Primary,
                },
                InlineButton {
                    label: "No".into(),
                    data: "no".into(),
                    style: ButtonStyle::Secondary,
                },
            ],
        };
        assert_eq!(
            rich.to_text_fallback(),
            "Choose one\n\nOptions: Yes | No"
        );
    }

    #[test]
    fn card_text_fallback() {
        let rich = RichContent::Card {
            title: "Title".into(),
            description: Some("Description".into()),
            image_url: None,
            url: Some("https://example.com".into()),
        };
        assert_eq!(
            rich.to_text_fallback(),
            "**Title**\nDescription\nhttps://example.com"
        );
    }

    #[test]
    fn serde_roundtrip() {
        let rich = RichContent::Image {
            url: "https://example.com/img.png".into(),
            caption: Some("test".into()),
        };
        let json = serde_json::to_string(&rich).unwrap();
        let parsed: RichContent = serde_json::from_str(&json).unwrap();
        assert_eq!(rich.to_text_fallback(), parsed.to_text_fallback());
    }
}
