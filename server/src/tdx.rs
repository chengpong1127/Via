use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;

use crate::{
    live_status::{Freshness, FreshnessPolicy, FreshnessReason, ObservationTimes},
    refresh::FeedSource,
};

const MAX_FEED_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TdxFeedConfig {
    pub capability: String,
    pub url: Url,
}

pub struct TdxFeedSource {
    capability: String,
    url: Url,
    client: Client,
    policy: FreshnessPolicy,
}

#[derive(Debug, thiserror::Error)]
pub enum TdxConfigError {
    #[error("TDX bearer token contains invalid header characters")]
    InvalidBearerToken(#[from] header::InvalidHeaderValue),
    #[error("TDX HTTP client configuration failed")]
    Client(#[from] reqwest::Error),
}

impl TdxFeedSource {
    pub fn new(config: TdxFeedConfig, bearer_token: &str) -> Result<Self, TdxConfigError> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {bearer_token}"))?,
        );
        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(8))
            .user_agent("via-server/1.0")
            .build()?;
        Ok(Self {
            capability: config.capability,
            url: config.url,
            client,
            policy: FreshnessPolicy::tdx_one_minute("1"),
        })
    }
}

#[async_trait]
impl FeedSource for TdxFeedSource {
    fn capability(&self) -> &str {
        &self.capability
    }

    async fn refresh(&self, evaluated_at: OffsetDateTime) -> Result<Freshness, FreshnessReason> {
        let response = self
            .client
            .get(self.url.clone())
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    FreshnessReason::TimedOut
                } else {
                    FreshnessReason::SourceError
                }
            })?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(FreshnessReason::RateLimited);
        }
        if !response.status().is_success() {
            return Err(FreshnessReason::SourceError);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_FEED_BYTES as u64)
        {
            return Err(FreshnessReason::RejectedPayload);
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| FreshnessReason::SourceError)?;
            if body.len().saturating_add(chunk.len()) > MAX_FEED_BYTES {
                return Err(FreshnessReason::RejectedPayload);
            }
            body.extend_from_slice(&chunk);
        }
        let payload: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| FreshnessReason::RejectedPayload)?;
        let published_at = newest_update_time(&payload).ok_or(FreshnessReason::RejectedPayload)?;
        Ok(self.policy.evaluate(
            ObservationTimes {
                measured_at: None,
                published_at: Some(published_at),
                received_at: evaluated_at,
                normalized_at: evaluated_at,
            },
            evaluated_at,
            None,
        ))
    }
}

fn newest_update_time(payload: &serde_json::Value) -> Option<OffsetDateTime> {
    let mut newest = None;
    collect_update_times(payload, &mut newest);
    newest
}

fn collect_update_times(payload: &serde_json::Value, newest: &mut Option<OffsetDateTime>) {
    match payload {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_update_times(value, newest);
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "UpdateTime" | "update_time") {
                    if let Some(parsed) = value
                        .as_str()
                        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
                    {
                        if newest.is_none_or(|current| parsed > current) {
                            *newest = Some(parsed);
                        }
                    }
                } else if value.is_array() || value.is_object() {
                    collect_update_times(value, newest);
                }
            }
        }
        _ => {}
    }
}
