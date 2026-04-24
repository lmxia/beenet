use spin_sdk::http::{IntoResponse, Request, Response};
use spin_sdk::http_component;

/// M1 example task: replace `badword` with `***`.
#[http_component]
fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    let text = String::from_utf8(req.into_body())?;
    let redacted = text.replace("badword", "***");
    Ok(Response::builder()
        .status(200)
        .header("content-type", "text/plain")
        .body(redacted)
        .build())
}
