//! LLM driver: one OpenAI-compatible `/chat/completions` call, no tools.
//!
//! Used for roles with `driver = "llm"` (cheap chat agents) and for triage
//! of incoming issues. Works with Ollama, LM Studio, llama.cpp, vLLM, MLX and
//! hosted OpenAI-compatible APIs.

use serde_json::{json, Value};
use std::time::Duration;
use tower_proto::Usage;

#[derive(Clone, Debug)]
pub struct LlmSpec {
    /// e.g. `http://localhost:11434/v1`.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

/// `(role, content)` pairs: "system", "user", "assistant".
pub type Messages = Vec<(String, String)>;

pub fn request_body(model: &str, messages: &Messages) -> Value {
    json!({
        "model": model,
        "messages": messages.iter().map(|(r, c)| json!({"role": r, "content": c})).collect::<Vec<_>>(),
        "temperature": 0.2,
        "stream": false,
    })
}

/// The reply text and usage from a chat completion response.
pub fn parse_response(v: &Value) -> Result<(String, Usage), String> {
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| err.to_string());
        return Err(msg);
    }
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or("the response has no choices[0].message.content")?
        .to_string();
    let n = |k: &str| {
        v.pointer(&format!("/usage/{k}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    Ok((
        text,
        Usage {
            input_tokens: n("prompt_tokens"),
            output_tokens: n("completion_tokens"),
            cost_usd: None,
        },
    ))
}

pub async fn chat(
    spec: &LlmSpec,
    messages: &Messages,
    timeout: Duration,
) -> Result<(String, Usage), String> {
    let url = format!("{}/chat/completions", spec.base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(&url).json(&request_body(&spec.model, messages));
    if let Some(key) = &spec.api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("{url}: HTTP {status}: unreadable response ({e})"))?;
    parse_response(&body).map_err(|e| format!("{url}: HTTP {status}: {e}"))
}

/// The first `{...}` JSON object in `text` (models like to wrap JSON in prose
/// or code fences).
pub fn first_json_object(text: &str) -> Option<Value> {
    let bytes = text.as_bytes();
    let mut start = None;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' if start.is_some() => in_str = true,
            b'{' => {
                if start.is_none() {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' if start.is_some() => {
                depth -= 1;
                if depth == 0 {
                    let s = start.take().unwrap_or(0);
                    if let Ok(v) = serde_json::from_str::<Value>(&text[s..=i]) {
                        if v.is_object() {
                            return Some(v);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_parse_with_usage_and_errors() {
        let ok = json!({"choices": [{"message": {"role": "assistant", "content": "hi"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3}});
        let (t, u) = parse_response(&ok).unwrap();
        assert_eq!(t, "hi");
        assert_eq!((u.input_tokens, u.output_tokens), (12, 3));
        assert_eq!(
            parse_response(&json!({"error": {"message": "model not found"}})).unwrap_err(),
            "model not found"
        );
        assert!(parse_response(&json!({})).is_err());
    }

    #[test]
    fn json_is_found_inside_prose_and_fences() {
        let t = "Sure!\n```json\n{\"role\": \"ground\", \"why\": \"a {small} fix\"}\n```";
        assert_eq!(first_json_object(t).unwrap()["role"], "ground");
        assert!(first_json_object("no json here").is_none());
        assert_eq!(first_json_object("{bad} then {\"a\": 1}").unwrap()["a"], 1);
    }

    #[tokio::test]
    async fn chat_talks_openai_compatible_http() {
        // A one-shot fake server.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.starts_with("POST /v1/chat/completions"), "{req}");
            assert!(req.to_lowercase().contains("authorization: bearer k"));
            let body = r#"{"choices":[{"message":{"content":"pong"}}],"usage":{"prompt_tokens":5,"completion_tokens":1}}"#;
            let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len());
            s.write_all(resp.as_bytes()).await.unwrap();
        });
        let spec = LlmSpec {
            base_url: format!("http://{addr}/v1"),
            model: "m".into(),
            api_key: Some("k".into()),
        };
        let (text, usage) = chat(
            &spec,
            &vec![("user".into(), "ping".into())],
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(text, "pong");
        assert_eq!(usage.total(), 6);
    }
}
