const SECRET_KEYS: [&str; 7] = ["p", "t", "s", "apiKey", "password", "token", "jwt"];

pub(crate) fn redact_query(query: &str) -> String {
    query
        .split('&')
        .map(|parameter| {
            let Some((key, value)) = parameter.split_once('=') else {
                return parameter.to_owned();
            };
            let decoded_key = form_urlencoded::parse(format!("{key}=").as_bytes())
                .next()
                .map(|(key, _)| key.into_owned())
                .unwrap_or_else(|| key.to_owned());
            if SECRET_KEYS
                .iter()
                .any(|secret| decoded_key.eq_ignore_ascii_case(secret))
            {
                format!("{key}=***")
            } else {
                format!("{key}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_values_case_insensitively_and_keeps_order() {
        assert_eq!(
            redact_query("id=1&p=pass&t=token&s=salt&apiKey=key&password=pw&token=tok&jwt=jwt"),
            "id=1&p=***&t=***&s=***&apiKey=***&password=***&token=***&jwt=***"
        );
        assert_eq!(
            redact_query("P=x&APIKEY=y&Password=z&JWT=w"),
            "P=***&APIKEY=***&Password=***&JWT=***"
        );
    }

    #[test]
    fn leaves_non_secret_empty_and_malformed_parameters_unchanged() {
        assert_eq!(redact_query("a=1&broken&b=2"), "a=1&broken&b=2");
        assert_eq!(redact_query(""), "");
        assert_eq!(redact_query("token"), "token");
        assert_eq!(redact_query("x=a=b&p="), "x=a=b&p=***");
        assert_eq!(
            redact_query("%70=encoded&%50assword=encoded"),
            "%70=***&%50assword=***"
        );
    }
}
