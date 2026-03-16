use async_trait::async_trait;
use super::{Tool, ToolContext, ToolOutput};
use crate::error::Result;

/// Email tool — send emails and check inbox using OAuth-connected accounts.
pub struct EmailTool;

impl EmailTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for EmailTool {
    fn name(&self) -> &str {
        "email"
    }

    fn description(&self) -> &str {
        "Send or read emails via Gmail or Microsoft Graph. Actions: send (to, subject, body), inbox (count?), search (query, count?)"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["send", "inbox", "search"],
                    "description": "Action to perform"
                },
                "to": {
                    "type": "string",
                    "description": "Recipient email address (for send)"
                },
                "subject": {
                    "type": "string",
                    "description": "Email subject (for send)"
                },
                "body": {
                    "type": "string",
                    "description": "Email body text (for send)"
                },
                "count": {
                    "type": "integer",
                    "description": "Number of emails to retrieve (default: 10)"
                },
                "provider": {
                    "type": "string",
                    "description": "Email provider: gmail or microsoft (optional, uses first available)"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let provider_pref = params.get("provider").and_then(|v| v.as_str());

        let platform = match provider_pref {
            Some("gmail") => "email_gmail",
            Some("microsoft") => "email_microsoft",
            _ => {
                if ctx.messaging.get("email_gmail").is_some() {
                    "email_gmail"
                } else if ctx.messaging.get("email_microsoft").is_some() {
                    "email_microsoft"
                } else {
                    return Ok(ToolOutput::error(
                        "No email backend configured. Connect a Gmail or Microsoft account via OAuth first."
                    ));
                }
            }
        };

        let backend = match ctx.messaging.get(platform) {
            Some(b) => b,
            None => {
                return Ok(ToolOutput::error(format!(
                    "Email backend '{platform}' not available"
                )));
            }
        };

        // Downcast to EmailBackend for provider-specific operations
        let email_backend = backend
            .as_any()
            .and_then(|a| a.downcast_ref::<crate::messaging::email::EmailBackend>());

        match action {
            "send" => {
                let to = params
                    .get("to")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let subject = params
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no subject)");
                let body = params
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                if to.is_empty() {
                    return Ok(ToolOutput::error("Missing 'to' parameter"));
                }
                if body.is_empty() {
                    return Ok(ToolOutput::error("Missing 'body' parameter"));
                }

                match email_backend {
                    Some(eb) => {
                        eb.send_email(to, subject, body).await?;
                        Ok(ToolOutput::ok(format!("Email sent to {to}: {subject}")))
                    }
                    None => {
                        backend.send_message(to, &format!("Subject: {subject}\n\n{body}")).await?;
                        Ok(ToolOutput::ok(format!("Email sent to {to}")))
                    }
                }
            }
            "inbox" => {
                let count = params
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10) as usize;

                match email_backend {
                    Some(eb) => {
                        let emails = eb.list_inbox(count).await?;
                        let summary: Vec<String> = emails
                            .iter()
                            .map(|e| {
                                format!(
                                    "{}[{}] From: {} — {}{}",
                                    if e.is_read { "" } else { "● " },
                                    e.date,
                                    e.from,
                                    e.subject,
                                    if e.snippet.is_empty() {
                                        String::new()
                                    } else {
                                        format!("\n  {}", &e.snippet[..e.snippet.len().min(120)])
                                    }
                                )
                            })
                            .collect();
                        Ok(ToolOutput::ok(format!(
                            "Inbox ({} emails):\n\n{}",
                            emails.len(),
                            summary.join("\n\n")
                        )))
                    }
                    None => Ok(ToolOutput::error(
                        "Inbox listing requires an email backend with OAuth tokens"
                    )),
                }
            }
            "search" => Ok(ToolOutput::error(
                "Email search not yet implemented. Use 'inbox' to list recent emails."
            )),
            _ => Ok(ToolOutput::error(format!(
                "Unknown email action '{action}'. Use: send, inbox, search"
            ))),
        }
    }
}
