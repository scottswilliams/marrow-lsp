use std::io::IsTerminal;

#[derive(Clone, Copy)]
pub enum Style {
    Notice,
    Error,
}

impl Style {
    fn ansi(self) -> &'static str {
        match self {
            Self::Notice => "\x1b[36m",
            Self::Error => "\x1b[31m",
        }
    }
}

pub fn paint_stderr(style: Style, text: impl AsRef<str>) -> String {
    paint_if(stderr_color_enabled(), style, text.as_ref())
}

fn stderr_color_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
        return false;
    }
    std::io::stderr().is_terminal()
}

fn paint_if(enabled: bool, style: Style, text: &str) -> String {
    if enabled {
        format!("{}{text}\x1b[0m", style.ansi())
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_enabled_wraps_text_in_ansi_style() {
        assert_eq!(
            paint_if(true, Style::Notice, "marrow-lsp:"),
            "\x1b[36mmarrow-lsp:\x1b[0m"
        );
        assert_eq!(
            paint_if(true, Style::Error, "error"),
            "\x1b[31merror\x1b[0m"
        );
    }

    #[test]
    fn paint_disabled_returns_plain_text() {
        assert_eq!(paint_if(false, Style::Notice, "marrow-lsp:"), "marrow-lsp:");
    }
}
