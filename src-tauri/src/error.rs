use serde::Serialize;

/// Machine-readable error classes for the IPC error envelope (B20). Only
/// errors the frontend must branch on carry a kind; every other error stays
/// a bare string on the wire — the historical shape all non-helper
/// consumers already handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// A voluntary user cancellation (file dialog, login window, a resolve
    /// superseded by a newer one): surfaces stay silent instead of toasting.
    Cancelled,
    /// A database access failed; the message carries mapped Chinese copy.
    Db,
    /// A filesystem access failed; the message carries mapped Chinese copy.
    Io,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),
    #[error("{user_message}")]
    Ipc {
        kind: ErrorKind,
        user_message: String,
    },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Rusqlite(#[from] rusqlite::Error),
}

impl AppError {
    /// Annotate an error the frontend must branch on (see [`ErrorKind`]).
    pub fn with_kind(kind: ErrorKind, user_message: impl Into<String>) -> Self {
        AppError::Ipc {
            kind,
            user_message: user_message.into(),
        }
    }

    /// The wire message: user-facing Chinese copy with the raw cause kept
    /// for diagnosis. `Message` and `Ipc` already carry final copy; the
    /// `#[from]` variants are where raw English leaks in.
    fn user_message(&self) -> String {
        match self {
            AppError::Message(m) => m.clone(),
            AppError::Ipc { user_message, .. } => user_message.clone(),
            AppError::Io(e) => format!("{}: {e}", io_kind_zh(e.kind())),
            AppError::Rusqlite(e) => format!("数据库错误: {e}"),
        }
    }
}

/// Chinese copy for the common std::io error classes; unknown kinds fall
/// back to a generic phrase with the original text still appended by the
/// caller.
fn io_kind_zh(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::NotFound => "路径不存在",
        std::io::ErrorKind::PermissionDenied => "没有访问权限",
        std::io::ErrorKind::AlreadyExists => "路径已存在",
        std::io::ErrorKind::InvalidInput => "输入无效",
        std::io::ErrorKind::InvalidData => "数据无效",
        std::io::ErrorKind::UnexpectedEof => "数据被截断",
        std::io::ErrorKind::TimedOut => "操作超时",
        std::io::ErrorKind::Interrupted => "操作被中断",
        std::io::ErrorKind::WriteZero => "写入失败",
        _ => "系统错误",
    }
}

// Hand-written, not derived: the wire shape is load-bearing (B20). The
// generic `Message` variant — every ordinary user-facing error — serializes
// as a bare string, the historical shape every existing consumer handles.
// Annotated variants (`Ipc`, `Io`, `Rusqlite`) serialize as {kind, message}
// so the frontend can branch programmatically; the frontend's
// `toErrorMessage`/`toApiError` helper is the one place that reads the
// envelope, and every invoke catch renders through it.
impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AppError::Message(m) => serializer.serialize_str(m),
            AppError::Ipc { kind, user_message } => envelope(serializer, *kind, user_message),
            AppError::Io(_) => envelope(serializer, ErrorKind::Io, &self.user_message()),
            AppError::Rusqlite(_) => envelope(serializer, ErrorKind::Db, &self.user_message()),
        }
    }
}

fn envelope<S: serde::Serializer>(
    serializer: S,
    kind: ErrorKind,
    message: &str,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct as _;
    let mut s = serializer.serialize_struct("ErrorEnvelope", 2)?;
    s.serialize_field("kind", &kind)?;
    s.serialize_field("message", message)?;
    s.end()
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind as IoKind;

    fn contract() -> serde_json::Value {
        serde_json::from_str(include_str!("../../src/types.contract.json"))
            .expect("src/types.contract.json must be valid JSON")
    }

    /// B20: the generic variant keeps the historical bare-string wire shape
    /// — every consumer that renders an error without the helper must keep
    /// working.
    #[test]
    fn generic_errors_serialize_as_a_bare_string() {
        let actual = serde_json::to_value(AppError::Message("保存失败".into())).unwrap();
        assert_eq!(
            actual,
            serde_json::json!("保存失败"),
            "a generic error must stay a bare string on the wire"
        );
    }

    /// B20: annotated variants serialize as the {kind, message} envelope,
    /// pinned against the golden fixture the TS contract test mirrors.
    #[test]
    fn the_error_envelope_matches_the_golden_fixture() {
        let actual =
            serde_json::to_value(AppError::with_kind(ErrorKind::Cancelled, "登录已取消")).unwrap();
        assert_eq!(
            actual,
            contract()["ErrorEnvelope"],
            "the envelope shape drifted from src/types.contract.json"
        );
    }

    /// B20: `#[from]` errors are where raw English leaks to the UI — the
    /// envelope maps them to Chinese copy with the original kept for
    /// diagnosis, and carries the matching kind.
    #[test]
    fn io_errors_map_to_chinese_copy_with_the_io_kind() {
        let err = AppError::Io(std::io::Error::new(IoKind::PermissionDenied, "os error 13"));
        let actual = serde_json::to_value(&err).unwrap();
        assert_eq!(actual["kind"], "io");
        let text = actual["message"].as_str().unwrap();
        assert!(
            text.starts_with("没有访问权限") && text.contains("os error 13"),
            "the message must lead with Chinese copy and keep the raw cause: {text}"
        );
    }

    #[test]
    fn unknown_io_kinds_fall_back_to_a_generic_phrase() {
        let err = AppError::Io(std::io::Error::other("weird failure"));
        let actual = serde_json::to_value(&err).unwrap();
        assert_eq!(actual["kind"], "io");
        let text = actual["message"].as_str().unwrap();
        assert!(
            text.starts_with("系统错误") && text.contains("weird failure"),
            "unknown kinds must fall back to the generic phrase with the cause: {text}"
        );
    }

    #[test]
    fn db_errors_map_to_the_db_kind() {
        let err = AppError::Rusqlite(rusqlite::Error::InvalidColumnName("x".into()));
        let actual = serde_json::to_value(&err).unwrap();
        assert_eq!(actual["kind"], "db");
        assert!(
            actual["message"]
                .as_str()
                .unwrap()
                .starts_with("数据库错误"),
            "the db message must lead with Chinese copy"
        );
    }

    #[test]
    fn ipc_errors_serialize_with_their_kind() {
        let err = AppError::with_kind(ErrorKind::Io, "自定义文案");
        let actual = serde_json::to_value(&err).unwrap();
        assert_eq!(actual["kind"], "io");
        assert_eq!(actual["message"], "自定义文案");
    }
}
