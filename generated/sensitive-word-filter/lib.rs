use serde::{Deserialize, Serialize};
use spin_sdk::http::{IntoResponse, Request, Response};
use spin_sdk::http_component;

const SENSITIVE_WORDS: &[&str] = &["诈骗", "赌博", "违禁品", "暴力"];
const MAX_TEXT_CHARS: usize = 20_000;

#[derive(Deserialize)]
struct FilterRequest {
    text: String,
}

#[derive(Serialize)]
struct Match {
    word: &'static str,
    count: usize,
}

#[derive(Serialize)]
struct FilterResponse {
    filtered_text: String,
    blocked: bool,
    total_matches: usize,
    matches: Vec<Match>,
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    error: &'a str,
}

fn json_response<T: Serialize>(status: u16, value: &T) -> Response {
    let body = serde_json::to_vec(value).expect("response serialization cannot fail");
    Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=utf-8")
        .body(body)
        .build()
}

#[http_component]
fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    let input = match serde_json::from_slice::<FilterRequest>(&req.into_body()) {
        Ok(input) => input,
        Err(_) => {
            return Ok(json_response(
                400,
                &ErrorResponse {
                    error: "invalid JSON request; expected a text field",
                },
            ))
        }
    };

    let text_chars = input.text.chars().count();
    if text_chars == 0 || text_chars > MAX_TEXT_CHARS {
        return Ok(json_response(
            422,
            &ErrorResponse {
                error: "text must contain between 1 and 20000 characters",
            },
        ));
    }

    let mut filtered_text = input.text;
    let mut matches = Vec::new();
    let mut total_matches = 0;
    for &word in SENSITIVE_WORDS {
        let count = filtered_text.matches(word).count();
        if count == 0 {
            continue;
        }
        let replacement = "*".repeat(word.chars().count());
        filtered_text = filtered_text.replace(word, &replacement);
        total_matches += count;
        matches.push(Match { word, count });
    }

    Ok(json_response(
        200,
        &FilterResponse {
            filtered_text,
            blocked: total_matches > 0,
            total_matches,
            matches,
        },
    ))
}
