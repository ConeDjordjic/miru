pub struct LogQuery {
    pub service_label: String,
    pub service: String,
    pub start: String,
    pub end: String,
    pub limit: u32,
    pub max_limit: u32,
    pub level: Option<String>,
    pub level_label: Option<String>,
    pub search: Option<String>,
    pub search_is_regex: bool,
}

fn escape_logql(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// A level goes into a backtick regex literal that can't be escaped, so
// restrict it to word characters.
fn validate_level(level: &str) -> anyhow::Result<()> {
    if level.is_empty()
        || !level
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        anyhow::bail!(
            "invalid level {level:?}: use a single level name like error, warn, crit, or trace"
        );
    }
    Ok(())
}

impl LogQuery {
    pub fn to_logql(&self) -> anyhow::Result<String> {
        if let Some(lvl) = &self.level {
            validate_level(lvl)?;
        }

        // label selector when configured, regex pipe otherwise
        let selector = match (&self.level, &self.level_label) {
            (Some(lvl), Some(label)) => format!(
                "{{{}=\"{}\", {}=\"{}\"}}",
                escape_logql(&self.service_label),
                escape_logql(&self.service),
                escape_logql(label),
                escape_logql(lvl)
            ),
            _ => format!(
                "{{{}=\"{}\"}}",
                escape_logql(&self.service_label),
                escape_logql(&self.service)
            ),
        };

        let after_level = match (&self.level, &self.level_label) {
            (Some(lvl), None) => format!("{} |~ `(?i)level[=:\"]+\\s*{}`", selector, lvl),
            _ => selector,
        };

        Ok(match &self.search {
            Some(s) if self.search_is_regex => {
                format!("{} |~ \"{}\"", after_level, escape_logql(s))
            }
            Some(s) => format!("{} |= \"{}\"", after_level, escape_logql(s)),
            None => after_level,
        })
    }

    pub fn to_params(&self) -> anyhow::Result<Vec<(String, String)>> {
        let effective_limit = self.limit.min(self.max_limit);
        Ok(vec![
            ("query".into(), self.to_logql()?),
            ("start".into(), self.start.clone()),
            ("end".into(), self.end.clone()),
            ("limit".into(), effective_limit.to_string()),
            ("direction".into(), "backward".into()),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_query() -> LogQuery {
        LogQuery {
            service_label: "app".into(),
            service: "auth".into(),
            start: "2026-06-12T13:00:00Z".into(),
            end: "2026-06-12T14:00:00Z".into(),
            limit: 50,
            max_limit: 1000,
            level: None,
            level_label: None,
            search: None,
            search_is_regex: false,
        }
    }

    #[test]
    fn logql_no_filter() {
        let q = base_query();
        assert_eq!(q.to_logql().unwrap(), r#"{app="auth"}"#);
    }

    #[test]
    fn logql_level_uses_regex_without_level_label() {
        let mut q = base_query();
        q.level = Some("error".into());
        let logql = q.to_logql().unwrap();
        assert_eq!(logql, "{app=\"auth\"} |~ `(?i)level[=:\"]+\\s*error`");
    }

    #[test]
    fn logql_level_uses_label_selector_with_level_label() {
        let mut q = base_query();
        q.level = Some("error".into());
        q.level_label = Some("level".into());
        let logql = q.to_logql().unwrap();
        assert_eq!(logql, r#"{app="auth", level="error"}"#);
    }

    #[test]
    fn logql_search_exact() {
        let mut q = base_query();
        q.search = Some("connection refused".into());
        let logql = q.to_logql().unwrap();
        assert_eq!(logql, r#"{app="auth"} |= "connection refused""#);
    }

    #[test]
    fn logql_search_regex() {
        let mut q = base_query();
        q.search = Some("timeout|refused".into());
        q.search_is_regex = true;
        let logql = q.to_logql().unwrap();
        assert_eq!(logql, r#"{app="auth"} |~ "timeout|refused""#);
    }

    #[test]
    fn logql_level_and_search_combined() {
        let mut q = base_query();
        q.level = Some("error".into());
        q.level_label = Some("level".into());
        q.search = Some("timeout".into());
        let logql = q.to_logql().unwrap();
        assert_eq!(logql, r#"{app="auth", level="error"} |= "timeout""#);
    }

    #[test]
    fn logql_level_regex_and_search_combined() {
        let mut q = base_query();
        q.level = Some("error".into());
        q.search = Some("timeout".into());
        let logql = q.to_logql().unwrap();
        assert!(logql.starts_with(r#"{app="auth"}"#), "got: {logql}");
        assert!(
            logql.contains("|~"),
            "expected level regex pipe, got: {logql}"
        );
        assert!(
            logql.contains("(?i)level"),
            "expected level regex, got: {logql}"
        );
        assert!(
            logql.contains("error"),
            "expected level value, got: {logql}"
        );
        assert!(
            logql.contains(r#"|= "timeout""#),
            "expected search pipe, got: {logql}"
        );
    }

    #[test]
    fn params_contain_expected_keys() {
        let q = base_query();
        let params = q.to_params().unwrap();
        assert_eq!(
            params
                .iter()
                .find(|(k, _)| k == "query")
                .map(|(_, v)| v.as_str()),
            Some(r#"{app="auth"}"#)
        );
        assert_eq!(
            params
                .iter()
                .find(|(k, _)| k == "start")
                .map(|(_, v)| v.as_str()),
            Some("2026-06-12T13:00:00Z")
        );
        assert_eq!(
            params
                .iter()
                .find(|(k, _)| k == "end")
                .map(|(_, v)| v.as_str()),
            Some("2026-06-12T14:00:00Z")
        );
        assert_eq!(
            params
                .iter()
                .find(|(k, _)| k == "limit")
                .map(|(_, v)| v.as_str()),
            Some("50")
        );
        assert_eq!(
            params
                .iter()
                .find(|(k, _)| k == "direction")
                .map(|(_, v)| v.as_str()),
            Some("backward")
        );
    }

    #[test]
    fn limit_clamped_to_max() {
        let mut q = base_query();
        q.limit = 9999;
        q.max_limit = 1000;
        let params = q.to_params().unwrap();
        assert_eq!(
            params
                .iter()
                .find(|(k, _)| k == "limit")
                .map(|(_, v)| v.as_str()),
            Some("1000")
        );
    }

    #[test]
    fn escaped_service_name_is_safe() {
        let mut q = base_query();
        q.service = r#"my"service"#.into();
        let logql = q.to_logql().unwrap();
        assert_eq!(logql, r#"{app="my\"service"}"#);
    }

    #[test]
    fn to_logql_rejects_invalid_level() {
        let mut q = base_query();
        q.level = Some("error` |~ `oops".into());
        // The builder owns the rule: an invalid level is rejected at
        // construction, not silently stripped.
        let err = q.to_logql().unwrap_err().to_string();
        assert!(err.contains("invalid level"), "got: {err}");
    }

    #[test]
    fn to_logql_accepts_valid_level() {
        let mut q = base_query();
        q.level = Some("error".into());
        assert_eq!(
            q.to_logql().unwrap(),
            "{app=\"auth\"} |~ `(?i)level[=:\"]+\\s*error`"
        );
    }
}
