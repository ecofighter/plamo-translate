use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct GlossaryEntry {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranslateRequest {
    pub text: String,
    pub target_lang: String,
    pub source_lang: Option<String>,
    pub context: Option<String>,
    pub glossary: Vec<GlossaryEntry>,
    pub form: Option<String>,
    pub stream: bool,
    pub max_tokens: Option<u32>,
}

impl Serialize for TranslateRequest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            v: u32,
            op: &'static str,
            text: &'a str,
            target_lang: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            source_lang: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            context: &'a Option<String>,
            #[serde(skip_serializing_if = "<[GlossaryEntry]>::is_empty")]
            glossary: &'a [GlossaryEntry],
            #[serde(skip_serializing_if = "Option::is_none")]
            form: &'a Option<String>,
            stream: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            max_tokens: &'a Option<u32>,
        }
        Wire {
            v: 1,
            op: "translate",
            text: &self.text,
            target_lang: &self.target_lang,
            source_lang: &self.source_lang,
            context: &self.context,
            glossary: &self.glossary,
            form: &self.form,
            stream: self.stream,
            max_tokens: &self.max_tokens,
        }
        .serialize(s)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Status {
    pub model: String,
    pub revision: String,
    pub loaded: bool,
    pub uptime_secs: u64,
    pub idle_timeout_secs: u64,
    pub queue_len: u32,
    pub version: String,
    pub protocol: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    ContextTooLong,
    Busy,
    ModelLoadFailed,
    ShuttingDown,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Progress {
        stage: String,
        done_bytes: Option<u64>,
        total_bytes: Option<u64>,
    },
    Delta {
        text: String,
    },
    Done {
        text: String,
        usage: Option<Usage>,
        model: Option<String>,
        revision: Option<String>,
    },
    Status(Status),
    Error {
        code: ErrorCode,
        message: String,
    },
    #[serde(other)]
    Unknown,
}

impl Event {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Event::Done { .. } | Event::Status(_) | Event::Error { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_with_v_and_op_and_omits_none() {
        let j = serde_json::to_value(TranslateRequest {
            text: "x".into(),
            target_lang: "en".into(),
            stream: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            j,
            serde_json::json!({"v":1,"op":"translate","text":"x","target_lang":"en","stream":true})
        );
    }
    #[test]
    fn unknown_event_type_is_unknown() {
        let e: Event = serde_json::from_str(r#"{"type":"future","a":1}"#).unwrap();
        assert_eq!(e, Event::Unknown);
    }
    #[test]
    fn unknown_error_code_is_unknown() {
        let e: Event =
            serde_json::from_str(r#"{"type":"error","code":"weird","message":"m"}"#).unwrap();
        assert_eq!(
            e,
            Event::Error {
                code: ErrorCode::Unknown,
                message: "m".into()
            }
        );
    }
    #[test]
    fn terminal_events() {
        assert!(!Event::Delta { text: "a".into() }.is_terminal());
        assert!(
            Event::Error {
                code: ErrorCode::Busy,
                message: String::new()
            }
            .is_terminal()
        );
    }
}
