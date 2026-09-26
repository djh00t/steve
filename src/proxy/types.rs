use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Logical request identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub Uuid);

/// Upstream attempt identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttemptId(pub Uuid);

/// Outcome of one upstream attempt. `Pending` means the attempt has started and not finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(not(test), allow(dead_code))]
pub enum AttemptStatus {
    Pending,
    UpstreamError,
    Success,
    Cancelled,
}

/// One logical client request, which may record several upstream attempts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    pub created_at: DateTime<Utc>,
    pub attempts: Vec<AttemptId>,
}

/// One upstream attempt for a logical request.
///
/// `provider` and `account` are string placeholders until Provider and UpstreamAccount exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestAttempt {
    pub id: AttemptId,
    pub request_id: RequestId,
    pub provider: String,
    pub account: String,
    pub status: AttemptStatus,
    pub started_at: DateTime<Utc>,
    /// Absent while the attempt is still in flight.
    pub finished_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use crate::proxy::{AttemptId, AttemptStatus, Request, RequestAttempt, RequestId};
    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    fn timestamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-26T08:34:00Z")
            .expect("fixed timestamp")
            .with_timezone(&Utc)
    }

    #[test]
    fn request_attempt_serde_round_trip() {
        let created_at = timestamp();
        let request_id = RequestId(Uuid::from_u128(1));
        let attempt_id = AttemptId(Uuid::from_u128(2));

        let request = Request {
            id: request_id,
            created_at,
            attempts: vec![attempt_id],
        };
        let attempt = RequestAttempt {
            id: attempt_id,
            request_id,
            provider: "anthropic".to_string(),
            account: "shared-api".to_string(),
            status: AttemptStatus::Success,
            started_at: created_at,
            finished_at: Some(created_at),
        };
        let pending = RequestAttempt {
            id: AttemptId(Uuid::from_u128(3)),
            request_id,
            provider: "openai".to_string(),
            account: "alice".to_string(),
            status: AttemptStatus::Pending,
            started_at: created_at,
            finished_at: None,
        };

        let request_json = serde_json::to_string(&request).expect("serialize request");
        let attempt_json = serde_json::to_string(&attempt).expect("serialize attempt");
        let pending_json = serde_json::to_string(&pending).expect("serialize pending attempt");

        let request_back: Request =
            serde_json::from_str(&request_json).expect("deserialize request");
        let attempt_back: RequestAttempt =
            serde_json::from_str(&attempt_json).expect("deserialize attempt");
        let pending_back: RequestAttempt =
            serde_json::from_str(&pending_json).expect("deserialize pending attempt");

        assert_eq!(request, request_back);
        assert_eq!(attempt, attempt_back);
        assert_eq!(pending, pending_back);

        assert_eq!(
            serde_json::to_value(AttemptStatus::Pending).expect("status"),
            serde_json::json!("pending")
        );
        assert_eq!(
            serde_json::to_value(AttemptStatus::UpstreamError).expect("status"),
            serde_json::json!("upstream_error")
        );
        assert_eq!(
            serde_json::to_value(AttemptStatus::Success).expect("status"),
            serde_json::json!("success")
        );
        assert_eq!(
            serde_json::to_value(AttemptStatus::Cancelled).expect("status"),
            serde_json::json!("cancelled")
        );

        for status in [
            AttemptStatus::Pending,
            AttemptStatus::UpstreamError,
            AttemptStatus::Success,
            AttemptStatus::Cancelled,
        ] {
            let encoded = serde_json::to_value(status).expect("serialize status");
            let decoded: AttemptStatus =
                serde_json::from_value(encoded).expect("deserialize status");
            assert_eq!(status, decoded);
        }
    }
}
