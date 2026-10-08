/// Internal assembly error. Most errors are reported and the pass continues;
/// a fatal error stops the pass because later lines would only report
/// follow-on errors (bank overflow, exceeded limits).
#[derive(Debug)]
pub(crate) struct AsmError {
    pub message: String,
    pub fatal: bool,
}

impl AsmError {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: true,
        }
    }
}

impl From<String> for AsmError {
    fn from(message: String) -> Self {
        Self {
            message,
            fatal: false,
        }
    }
}

impl From<&str> for AsmError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

pub(crate) type AsmResult<T> = Result<T, AsmError>;
