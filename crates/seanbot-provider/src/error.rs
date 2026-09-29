use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("认证失败（HTTP {status}）：{body}")]
    Auth { status: u16, body: String },
    #[error("请求过于频繁（HTTP 429）")]
    RateLimited { retry_after: Option<Duration> },
    #[error("服务端错误（HTTP {status}）：{body}")]
    Server { status: u16, body: String },
    #[error("网络错误：{0}")]
    Network(String),
    #[error("请求无效：{0}")]
    BadRequest(String),
    #[error("响应解析失败：{0}")]
    Decode(String),
}

impl ProviderError {
    /// 按 HTTP 状态码归类非 2xx 响应。
    pub fn from_status(status: u16, body: String, retry_after: Option<Duration>) -> Self {
        match status {
            401 | 403 => Self::Auth { status, body },
            429 => Self::RateLimited { retry_after },
            500..=599 => Self::Server { status, body },
            _ => Self::BadRequest(format!("HTTP {status}：{body}")),
        }
    }

    /// 仅限流、服务端错误与网络错误可以重试。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::Server { .. } | Self::Network(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_status_codes() {
        assert!(matches!(
            ProviderError::from_status(401, "x".into(), None),
            ProviderError::Auth { status: 401, .. }
        ));
        assert!(matches!(
            ProviderError::from_status(403, "x".into(), None),
            ProviderError::Auth { .. }
        ));
        assert_eq!(
            ProviderError::from_status(429, "x".into(), Some(Duration::from_secs(2))),
            ProviderError::RateLimited {
                retry_after: Some(Duration::from_secs(2))
            }
        );
        assert!(matches!(
            ProviderError::from_status(503, "x".into(), None),
            ProviderError::Server { status: 503, .. }
        ));
        assert!(
            matches!(ProviderError::from_status(400, "too long".into(), None), ProviderError::BadRequest(m) if m.contains("too long"))
        );
        assert!(matches!(
            ProviderError::from_status(402, "余额不足".into(), None),
            ProviderError::BadRequest(_)
        ));
    }

    #[test]
    fn retryable_classification() {
        assert!(ProviderError::RateLimited { retry_after: None }.is_retryable());
        assert!(
            ProviderError::Server {
                status: 500,
                body: String::new()
            }
            .is_retryable()
        );
        assert!(ProviderError::Network("reset".into()).is_retryable());
        assert!(
            !ProviderError::Auth {
                status: 401,
                body: String::new()
            }
            .is_retryable()
        );
        assert!(!ProviderError::BadRequest(String::new()).is_retryable());
        assert!(!ProviderError::Decode(String::new()).is_retryable());
    }
}
