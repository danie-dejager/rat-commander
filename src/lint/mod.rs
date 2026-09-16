//! Syntax checks for the text formats the editor checks as they are typed:
//! JSON and its relatives (see [`crate::json`]), TOML, YAML and XML. Each
//! reports the errors it finds as [`Diagnostic`]s: a byte span on one line and
//! what is wrong there.

mod toml;
mod xml;
mod yaml;

pub use crate::json::Diagnostic;

/// Which language a file is checked as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Json(crate::json::Options),
    Toml,
    Yaml,
    Xml,
}

/// How a file named `name` is checked, or `None` when it isn't.
pub fn lang_for_name(name: &str) -> Option<Lang> {
    if let Some(opts) = crate::json::options_for_name(name) {
        return Some(Lang::Json(opts));
    }
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "toml" => Some(Lang::Toml),
        "yaml" | "yml" => Some(Lang::Yaml),
        "xml" | "xsd" | "xsl" | "xslt" | "svg" | "csproj" | "vbproj" | "fsproj" | "props"
        | "targets" | "xaml" | "kml" | "gpx" | "rss" | "atom" | "plist" | "xhtml" | "wsdl" => {
            Some(Lang::Xml)
        }
        _ if lower == "cargo.lock" || lower == "poetry.lock" || lower == "uv.lock" => {
            Some(Lang::Toml)
        }
        _ => None,
    }
}

/// The errors in `text`, checked as `lang`, in text order.
pub fn check(lang: Lang, text: &str) -> Vec<Diagnostic> {
    let mut diags = match lang {
        Lang::Json(opts) => crate::json::parse(text, opts, &mut crate::json::NullSink),
        Lang::Toml => toml::check(text),
        Lang::Yaml => yaml::check(text),
        Lang::Xml => xml::check(text),
    };
    diags.sort_by_key(|d| d.span.start);
    diags
}

/// A diagnostic at `span` of `text`, cut to one line.
fn at(text: &str, span: std::ops::Range<usize>, message: impl Into<String>) -> Diagnostic {
    Diagnostic { span: crate::json::clip_to_line(text, span), message: message.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_go_by_name() {
        assert!(matches!(lang_for_name("data.json"), Some(Lang::Json(_))));
        assert_eq!(lang_for_name("Cargo.toml"), Some(Lang::Toml));
        assert_eq!(lang_for_name("Cargo.lock"), Some(Lang::Toml));
        assert_eq!(lang_for_name("docker-compose.YML"), Some(Lang::Yaml));
        assert_eq!(lang_for_name("app.csproj"), Some(Lang::Xml));
        assert_eq!(lang_for_name("icon.svg"), Some(Lang::Xml));
        assert_eq!(lang_for_name("notes.txt"), None);
        assert_eq!(lang_for_name("index.html"), None, "HTML isn't XML");
    }
}
