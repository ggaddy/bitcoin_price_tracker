use std::{
    error::Error,
    fmt,
    time::{Duration, SystemTime},
};

use reqwest::{StatusCode, header::HeaderValue};

use crate::pricing::UpstreamSource;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RetryAfter {
    Delay(Duration),
    At(SystemTime),
}

impl RetryAfter {
    pub(crate) fn parse(value: &HeaderValue) -> Option<Self> {
        let value = value.to_str().ok()?.trim();
        if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
            return value
                .parse::<u64>()
                .ok()
                .map(|seconds| Self::Delay(Duration::from_secs(seconds)));
        }
        httpdate::parse_http_date(value).ok().map(Self::At)
    }
}

#[derive(Debug)]
pub(crate) enum ProviderErrorKind {
    Timeout(reqwest::Error),
    Transport(reqwest::Error),
    Http {
        status: StatusCode,
        retry_after: Option<RetryAfter>,
    },
    InvalidPayload {
        reason: &'static str,
        cause: Option<reqwest::Error>,
    },
    InvalidPrice {
        field: &'static str,
        reason: &'static str,
    },
}

#[derive(Debug)]
pub(crate) struct ProviderError {
    pub(crate) provider: UpstreamSource,
    pub(crate) kind: ProviderErrorKind,
}

impl ProviderError {
    pub(crate) fn from_reqwest(provider: UpstreamSource, error: reqwest::Error) -> Self {
        // Body reads can time out too; classify these before JSON decode failures.
        let kind = if error.is_timeout() {
            ProviderErrorKind::Timeout(error)
        } else if Self::has_json_cause(&error) {
            ProviderErrorKind::InvalidPayload {
                reason: "invalid JSON",
                cause: Some(error),
            }
        } else {
            ProviderErrorKind::Transport(error)
        };
        Self { provider, kind }
    }

    fn has_json_cause(error: &reqwest::Error) -> bool {
        // Reqwest also wraps failed body transfers as Decode errors; only a JSON
        // parser cause means that the provider sent an invalid JSON payload.
        let mut cause = error.source();
        while let Some(current) = cause {
            if current.is::<serde_json::Error>() {
                return true;
            }
            cause = current.source();
        }
        false
    }

    pub(crate) fn invalid_payload(provider: UpstreamSource, reason: &'static str) -> Self {
        Self {
            provider,
            kind: ProviderErrorKind::InvalidPayload {
                reason,
                cause: None,
            },
        }
    }

    pub(crate) fn invalid_price(
        provider: UpstreamSource,
        field: &'static str,
        reason: &'static str,
    ) -> Self {
        Self {
            provider,
            kind: ProviderErrorKind::InvalidPrice { field, reason },
        }
    }
}

// Display is safe for API warnings; Debug and the error chain retain diagnostic causes for logs.
impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ", self.provider.name())?;
        match &self.kind {
            ProviderErrorKind::Timeout(_) => write!(f, "request timed out"),
            ProviderErrorKind::Transport(_) => write!(f, "request failed"),
            ProviderErrorKind::Http {
                status,
                retry_after,
            } => {
                write!(f, "HTTP error: {status}")?;
                match retry_after {
                    Some(RetryAfter::Delay(delay)) => {
                        write!(f, "; retry after {}s", delay.as_secs())
                    }
                    Some(RetryAfter::At(time)) => {
                        write!(f, "; retry at {}", httpdate::fmt_http_date(*time))
                    }
                    None => Ok(()),
                }
            }
            ProviderErrorKind::InvalidPayload { reason, .. } => {
                write!(f, "response invalid: {reason}")
            }
            ProviderErrorKind::InvalidPrice { field, reason } => {
                write!(f, "price invalid at {field}: {reason}")
            }
        }
    }
}

impl Error for ProviderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            ProviderErrorKind::Timeout(error) | ProviderErrorKind::Transport(error) => Some(error),
            ProviderErrorKind::InvalidPayload {
                cause: Some(error), ..
            } => Some(error),
            _ => None,
        }
    }
}

pub(crate) enum RefreshError {
    Provider(ProviderError),
    Storage(String),
}

// Explicitly retain storage details for diagnostic logs, while Display stays safe for clients.
impl fmt::Debug for RefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => f.debug_tuple("Provider").field(error).finish(),
            Self::Storage(details) => f.debug_tuple("Storage").field(details).finish(),
        }
    }
}

impl From<ProviderError> for RefreshError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

impl fmt::Display for RefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(f),
            Self::Storage(_) => write!(f, "Failed to store refreshed price data"),
        }
    }
}

impl Error for RefreshError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider(error) => Some(error),
            Self::Storage(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RefreshError, RetryAfter};
    use reqwest::header::HeaderValue;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn retry_after_preserves_delay_and_absolute_date() {
        assert_eq!(
            RetryAfter::parse(&HeaderValue::from_static("20")),
            Some(RetryAfter::Delay(Duration::from_secs(20)))
        );
        assert_eq!(
            RetryAfter::parse(&HeaderValue::from_static("0")),
            Some(RetryAfter::Delay(Duration::ZERO))
        );
        assert_eq!(
            RetryAfter::parse(&HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT")),
            Some(RetryAfter::At(
                UNIX_EPOCH + Duration::from_secs(1_445_412_480)
            ))
        );
    }

    #[test]
    fn retry_after_ignores_invalid_and_overflowing_values() {
        for value in ["", "-1", "+1", "1.5", "tomorrow", "18446744073709551616"] {
            assert_eq!(
                RetryAfter::parse(&HeaderValue::from_str(value).unwrap()),
                None,
                "{value}"
            );
        }
        assert_eq!(
            RetryAfter::parse(&HeaderValue::from_bytes(&[0xff]).unwrap()),
            None
        );
    }

    #[test]
    fn storage_details_are_diagnostic_only() {
        let error = RefreshError::Storage("database failed at /private/fixture.db".to_string());
        assert!(!error.to_string().contains("/private/fixture.db"));
        assert!(format!("{error:?}").contains("/private/fixture.db"));
    }
}
