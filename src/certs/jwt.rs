//! JSON Web Tokens: the token under the editor's cursor, its header and
//! claims decoded and laid out, the times in it read as dates. The signature
//! is not checked — there is no key to check it with.

use base64::Engine;

/// Characters a token is made of: base64url and the dots between its parts.
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// The token around char `col` of `line`: the longest run of token
/// characters there, if it has the three parts of a JWT and a JSON header.
pub fn token_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let at = col.min(chars.len().saturating_sub(1));
    if !chars.get(at).is_some_and(|&c| token_char(c)) {
        return None;
    }
    let start = chars[..at].iter().rposition(|&c| !token_char(c)).map_or(0, |i| i + 1);
    let end = chars[at..].iter().position(|&c| !token_char(c)).map_or(chars.len(), |i| at + i);
    let token: String = chars[start..end].iter().collect();
    let mut parts: Vec<&str> = token.trim_start_matches('.').split('.').collect();
    // An unsigned token ends in its dot; a sentence can add one more.
    if parts.len() == 4 && parts[3].is_empty() {
        parts.pop();
    }
    // A header is JSON, so it starts `{"`: `eyJ` in base64.
    (parts.len() == 3 && parts[0].starts_with("eyJ") && !parts[1].is_empty())
        .then(|| parts.join("."))
}

fn part(text: &str) -> Result<serde_json::Value, String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| "not base64url".to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "not JSON".to_string())
}

/// The token decoded for reading, as of `now` (Unix time).
pub fn decode(token: &str, now: i64) -> Result<String, String> {
    let parts: Vec<&str> = token.split('.').collect();
    let [header, payload, signature] = parts[..] else {
        return Err("a JWT has three parts".into());
    };
    let header = part(header).map_err(|e| format!("The header is {e}"))?;
    let claims = part(payload).map_err(|e| format!("The payload is {e}"))?;
    let pretty = |v: &serde_json::Value| serde_json::to_string_pretty(v).unwrap_or_default();
    let mut out = format!("Header\n{}\n\nPayload\n{}\n", pretty(&header), pretty(&claims));
    let times: Vec<String> = [("iat", "issued"), ("nbf", "not before"), ("exp", "expires")]
        .iter()
        .filter_map(|(key, what)| {
            let t = claims.get(key)?.as_i64()?;
            let days = (t - now).div_euclid(86_400);
            let when = match *key {
                "exp" if t < now => format!("  (expired {} days ago)", (now - t) / 86_400),
                "exp" => format!("  (in {days} days)"),
                "nbf" if t > now => format!("  (not valid for another {days} days)"),
                _ => String::new(),
            };
            Some(format!("{what:<11} {}{when}", super::x509::date(t)))
        })
        .collect();
    if !times.is_empty() {
        out.push_str(&format!("\nTimes\n{}\n", times.join("\n")));
    }
    let alg = header.get("alg").and_then(|a| a.as_str()).unwrap_or("?");
    let signed = if signature.is_empty() || alg.eq_ignore_ascii_case("none") {
        "The token is not signed.".to_string()
    } else {
        format!("Signed with {alg}; the signature is not checked.")
    };
    out.push_str(&format!("\n{signed}\n"));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// jwt.io's example token: `{"alg":"HS256","typ":"JWT"}`,
    /// `{"sub":"1234567890","name":"John Doe","iat":1516239022}`.
    const EXAMPLE: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";

    #[test]
    fn the_token_is_found_wherever_the_cursor_is_in_it() {
        let line = format!("Authorization: Bearer {EXAMPLE}\"");
        let start = line.find("eyJ").unwrap();
        assert_eq!(token_at(&line, start).as_deref(), Some(EXAMPLE));
        assert_eq!(token_at(&line, start + 40).as_deref(), Some(EXAMPLE));
        assert_eq!(token_at(&line, line.len() - 2).as_deref(), Some(EXAMPLE));
        assert_eq!(token_at(&line, 3), None, "on `Authorization`");
        assert_eq!(token_at(&line, start - 1), None, "on the space");
        assert_eq!(token_at("version 1.2.3", 9), None, "three parts, but not a token's");
        let unsigned = format!("{}.", EXAMPLE.rsplit_once('.').unwrap().0);
        assert_eq!(
            token_at(&unsigned, 5).as_deref(),
            Some(unsigned.as_str()),
            "its empty signature kept"
        );
        let sentence = format!("Use {EXAMPLE}.");
        assert_eq!(
            token_at(&sentence, 10).as_deref(),
            Some(EXAMPLE),
            "the full stop isn't part of it"
        );
    }

    #[test]
    fn a_token_decodes_with_its_times_and_says_its_signature_is_unchecked() {
        let text = decode(EXAMPLE, 1_516_239_022 + 10 * 86_400).unwrap();
        assert!(text.contains("\"alg\": \"HS256\""), "{text}");
        assert!(text.contains("\"name\": \"John Doe\""), "{text}");
        assert!(text.contains("issued      2018-01-18 01:30:22 UTC"), "{text}");
        assert!(text.contains("Signed with HS256; the signature is not checked."), "{text}");
        let unsigned = format!("{}.", EXAMPLE.rsplit_once('.').unwrap().0);
        assert!(decode(&unsigned, 0).unwrap().contains("not signed"));
        assert!(decode("abc.def.ghi", 0).unwrap_err().starts_with("The header is"));
    }
}
