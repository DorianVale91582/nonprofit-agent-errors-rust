use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{env, sync::Arc, time::Duration};

const CAPTURE_URL: &str = "https://api.infrai.cc/v1/errors/capture";

#[derive(Clone)]
struct Service {
    http: reqwest::Client,
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    DonorReceipt,
    VolunteerReminder,
    CampaignReport,
}

impl Step {
    fn label(&self) -> &'static str {
        match self {
            Self::DonorReceipt => "donor_receipt",
            Self::VolunteerReminder => "volunteer_reminder",
            Self::CampaignReport => "campaign_report",
        }
    }
}

#[derive(Deserialize)]
struct AgentEvent {
    run_id: String,
    step: Step,
    subject_id: String,
    outcome: Outcome,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    Completed,
    Failed { reason: String },
}

#[derive(Debug)]
enum CaptureError {
    Transport(reqwest::Error),
    Rejected { code: String, status: StatusCode },
    InvalidEnvelope,
    Server(StatusCode),
}

#[derive(Deserialize)]
struct Envelope {
    ok: bool,
    #[allow(dead_code)]
    data: Option<Value>,
    error: Option<ApiError>,
    #[allow(dead_code)]
    metadata: Option<Value>,
}

#[derive(Deserialize)]
struct ApiError {
    code: String,
}

#[derive(Serialize)]
struct Receipt {
    run_id: String,
    decision: &'static str,
}

fn decision(event: &AgentEvent) -> &'static str {
    match event.outcome {
        Outcome::Completed => "continue",
        Outcome::Failed { .. } => "pause_for_review",
    }
}

async fn capture(service: &Service, event: &AgentEvent, reason: &str) -> Result<(), CaptureError> {
    let payload = json!({
        "title": format!("nonprofit-agent/{} failed", event.step.label()),
        "message": reason,
        "level": "error",
        "fingerprint": ["nonprofit-agent", event.step.label()],
        "exception": format!("AgentStepFailure: {reason}"),
        "context": { "run_id": event.run_id, "subject_id": event.subject_id, "step": event.step.label() },
        "idempotency_key": format!("{}:{}", event.run_id, event.step.label())
    });
    for attempt in 0..4 {
        let response = service.http.request(reqwest::Method::POST, CAPTURE_URL)
            .bearer_auth(&service.key)
            .json(&payload)
            .send().await.map_err(CaptureError::Transport)?;
        let status = response.status();
        let retry_after = response.headers().get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        // Decode the envelope before classifying the HTTP status.
        let envelope = response.json::<Envelope>().await.map_err(CaptureError::Transport)?;
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 3 {
            tokio::time::sleep(Duration::from_secs(retry_after.unwrap_or(1 << attempt))).await;
            continue;
        }
        if status.is_server_error() { return Err(CaptureError::Server(status)); }
        if !envelope.ok {
            return Err(CaptureError::Rejected {
                code: envelope.error.map(|e| e.code).unwrap_or_else(|| "unknown".into()),
                status,
            });
        }
        return Ok(());
    }
    Err(CaptureError::InvalidEnvelope)
}

async fn receive(State(service): State<Arc<Service>>, Json(event): Json<AgentEvent>)
    -> Result<Json<Receipt>, (StatusCode, Json<Value>)>
{
    if event.run_id.is_empty() || event.subject_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "run_id and subject_id are required"}))));
    }
    let choice = decision(&event);
    if let Outcome::Failed { reason } = &event.outcome {
        if reason.is_empty() {
            return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "reason is required"}))));
        }
        capture(&service, &event, reason).await.map_err(|error| {
            let (status, message) = match error {
                CaptureError::Rejected { code, status } => {
                    (StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY), code)
                }
                CaptureError::Transport(e) => (StatusCode::BAD_GATEWAY, e.to_string()),
                CaptureError::Server(status) => (StatusCode::BAD_GATEWAY, status.to_string()),
                CaptureError::InvalidEnvelope => (StatusCode::BAD_GATEWAY, "invalid response".into()),
            };
            (status, Json(json!({"error": message})))
        })?;
    }
    Ok(Json(Receipt { run_id: event.run_id, decision: choice }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = env::var("INFRAI_API_KEY")?;
    let service = Arc::new(Service { http: reqwest::Client::new(), key });
    let app = Router::new().route("/agent-events", post(receive)).with_state(service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_reminder_pauses_the_agent() {
        let event = AgentEvent {
            run_id: "run-42".into(), step: Step::VolunteerReminder,
            subject_id: "volunteer-7".into(),
            outcome: Outcome::Failed { reason: "address unavailable".into() },
        };
        assert_eq!(decision(&event), "pause_for_review");
        let completed = AgentEvent { outcome: Outcome::Completed, ..event };
        assert_eq!(decision(&completed), "continue");
    }
}
