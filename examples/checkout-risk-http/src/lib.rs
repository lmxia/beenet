#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use serde::{Deserialize, Serialize};
use spin_sdk::http::{IntoResponse, Request, Response};
#[cfg(target_arch = "wasm32")]
use spin_sdk::http_component;

const POLICY_VERSION: &str = "checkout-risk/2026-07-21";

#[derive(Debug, Deserialize)]
struct Checkout {
    order_id: String,
    amount: f64,
    currency: String,
    account_age_days: u32,
    billing_country: String,
    shipping_country: String,
    ip_country: String,
    failed_payment_attempts: u32,
    expedited_shipping: bool,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Decision {
    Approve,
    Review,
    Decline,
}

#[derive(Debug, Serialize)]
struct Assessment {
    order_id: String,
    decision: Decision,
    risk_score: u8,
    reasons: Vec<&'static str>,
    policy_version: &'static str,
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    error: &'a str,
}

fn validate(checkout: &Checkout) -> Result<(), &'static str> {
    if checkout.order_id.trim().is_empty() {
        return Err("order_id must not be empty");
    }
    if checkout.amount <= 0.0 {
        return Err("amount must be greater than zero");
    }
    if checkout.currency.trim().len() != 3 {
        return Err("currency must be a three-letter code");
    }
    for country in [
        &checkout.billing_country,
        &checkout.shipping_country,
        &checkout.ip_country,
    ] {
        if country.trim().len() != 2 {
            return Err("countries must use two-letter codes");
        }
    }
    Ok(())
}

fn assess(checkout: Checkout) -> Assessment {
    let mut score = 0_u8;
    let mut reasons = Vec::new();

    if checkout.amount >= 5_000.0 {
        score += 40;
        reasons.push("very_high_order_value");
    } else if checkout.amount >= 1_000.0 {
        score += 20;
        reasons.push("high_order_value");
    }

    if checkout.account_age_days < 7 {
        score += 25;
        reasons.push("new_account");
    } else if checkout.account_age_days < 30 {
        score += 10;
        reasons.push("young_account");
    }

    if !checkout
        .billing_country
        .eq_ignore_ascii_case(&checkout.shipping_country)
    {
        score += 15;
        reasons.push("billing_shipping_country_mismatch");
    }
    if !checkout
        .billing_country
        .eq_ignore_ascii_case(&checkout.ip_country)
    {
        score += 20;
        reasons.push("billing_ip_country_mismatch");
    }

    if checkout.failed_payment_attempts >= 3 {
        score += 30;
        reasons.push("repeated_payment_failures");
    } else if checkout.failed_payment_attempts > 0 {
        score += (checkout.failed_payment_attempts * 5) as u8;
        reasons.push("previous_payment_failure");
    }

    if checkout.expedited_shipping && checkout.amount >= 1_000.0 {
        score += 10;
        reasons.push("expedited_high_value_order");
    }

    let risk_score = score.min(100);
    let decision = if checkout.failed_payment_attempts >= 5 || risk_score >= 70 {
        Decision::Decline
    } else if risk_score >= 35 {
        Decision::Review
    } else {
        Decision::Approve
    };

    if reasons.is_empty() {
        reasons.push("no_risk_signals");
    }

    Assessment {
        order_id: checkout.order_id,
        decision,
        risk_score,
        reasons,
        policy_version: POLICY_VERSION,
    }
}

fn json_response<T: Serialize>(status: u16, value: &T) -> Response {
    let body = serde_json::to_vec(value).expect("serializing a response cannot fail");
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("x-policy-version", POLICY_VERSION)
        .body(body)
        .build()
}

#[cfg_attr(target_arch = "wasm32", http_component)]
fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    let checkout = match serde_json::from_slice::<Checkout>(&req.into_body()) {
        Ok(checkout) => checkout,
        Err(_) => {
            return Ok(json_response(
                400,
                &ErrorResponse {
                    error: "invalid JSON request",
                },
            ))
        }
    };
    if let Err(error) = validate(&checkout) {
        return Ok(json_response(422, &ErrorResponse { error }));
    }

    Ok(json_response(200, &assess(checkout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout() -> Checkout {
        Checkout {
            order_id: "ord_123".to_string(),
            amount: 99.0,
            currency: "USD".to_string(),
            account_age_days: 300,
            billing_country: "US".to_string(),
            shipping_country: "US".to_string(),
            ip_country: "US".to_string(),
            failed_payment_attempts: 0,
            expedited_shipping: false,
        }
    }

    #[test]
    fn approves_a_low_risk_checkout() {
        let result = assess(checkout());

        assert_eq!(result.decision, Decision::Approve);
        assert_eq!(result.risk_score, 0);
        assert_eq!(result.reasons, ["no_risk_signals"]);
    }

    #[test]
    fn reviews_a_checkout_with_multiple_signals() {
        let mut input = checkout();
        input.amount = 1_500.0;
        input.account_age_days = 10;
        input.ip_country = "CA".to_string();

        let result = assess(input);

        assert_eq!(result.decision, Decision::Review);
        assert_eq!(result.risk_score, 50);
    }

    #[test]
    fn declines_repeated_payment_failures() {
        let mut input = checkout();
        input.failed_payment_attempts = 5;

        let result = assess(input);

        assert_eq!(result.decision, Decision::Decline);
        assert_eq!(result.risk_score, 30);
    }

    #[test]
    fn rejects_invalid_country_codes() {
        let mut input = checkout();
        input.ip_country = "USA".to_string();

        assert_eq!(validate(&input), Err("countries must use two-letter codes"));
    }
}
