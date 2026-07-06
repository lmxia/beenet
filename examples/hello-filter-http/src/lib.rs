use spin_sdk::http::{IntoResponse, Request, Response};
use spin_sdk::http_component;

fn classify(text: &str) -> (&'static str, &'static str) {
    let lower = text.to_ascii_lowercase();
    if lower.contains("refund") || lower.contains("invoice") || lower.contains("billing") {
        ("billing", "route to finance")
    } else if lower.contains("urgent") || lower.contains("outage") || lower.contains("down") {
        ("incident", "escalate now")
    } else if lower.contains("bug") || lower.contains("error") || lower.contains("crash") {
        ("bug", "create a fix ticket")
    } else if lower.contains("feature") || lower.contains("request") {
        ("feature", "capture roadmap idea")
    } else {
        ("general", "reply normally")
    }
}

/// AI-ready demo task: classify a short customer request into a support lane.
#[http_component]
fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    let text = String::from_utf8(req.into_body())?;
    let (label, action) = classify(&text);
    let body = format!(
        "{{\"label\":\"{label}\",\"action\":\"{action}\",\"summary\":\"{}\"}}",
        text.trim().replace('\"', "'")
    );
    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(body)
        .build())
}
