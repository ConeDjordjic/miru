use chrono::DateTime;

pub struct MetricRequest {
    pub promql: String,
    pub start: String,
    pub end: String,
    pub step: Option<u32>,
    pub instant: bool,
}

fn window_seconds(start: &str, end: &str) -> Option<i64> {
    let s = DateTime::parse_from_rfc3339(start).ok()?;
    let e = DateTime::parse_from_rfc3339(end).ok()?;
    Some((e - s).num_seconds())
}

pub(crate) fn resolve_step(
    start: &str,
    end: &str,
    requested: Option<u32>,
    target_points: u32,
    min_step: u32,
) -> u32 {
    if let Some(s) = requested {
        return s.max(1);
    }
    let min_step = min_step.max(1);
    match window_seconds(start, end) {
        Some(w) if w > 0 => {
            let target = target_points.max(1) as i64;
            let step = (w / target).max(1) as u32;
            step.max(min_step)
        }
        _ => min_step,
    }
}

impl MetricRequest {
    pub(crate) fn to_params(&self, target_points: u32, min_step: u32) -> Vec<(String, String)> {
        if self.instant {
            vec![
                ("query".into(), self.promql.clone()),
                ("time".into(), self.end.clone()),
            ]
        } else {
            let step = resolve_step(&self.start, &self.end, self.step, target_points, min_step);
            vec![
                ("query".into(), self.promql.clone()),
                ("start".into(), self.start.clone()),
                ("end".into(), self.end.clone()),
                ("step".into(), step.to_string()),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range_request() -> MetricRequest {
        MetricRequest {
            promql: "rate(http_requests_total[5m])".into(),
            start: "2026-06-12T13:00:00Z".into(),
            end: "2026-06-12T14:00:00Z".into(),
            step: None,
            instant: false,
        }
    }

    #[test]
    fn resolve_step_prefers_requested() {
        assert_eq!(
            resolve_step("2026-06-12T13:00:00Z", "2026-06-12T14:00:00Z", Some(45), 100, 15),
            45
        );
    }

    #[test]
    fn resolve_step_computes_from_window() {
        // 1h / 100 target points = 36s, above the 15s floor.
        assert_eq!(
            resolve_step("2026-06-12T13:00:00Z", "2026-06-12T14:00:00Z", None, 100, 15),
            36
        );
    }

    #[test]
    fn resolve_step_respects_min_step_floor() {
        // 1m / 100 would be sub-second, so clamp up to the floor.
        assert_eq!(
            resolve_step("2026-06-12T13:00:00Z", "2026-06-12T13:01:00Z", None, 100, 15),
            15
        );
    }

    #[test]
    fn resolve_step_falls_back_when_unparseable() {
        assert_eq!(resolve_step("not-a-date", "also-not", None, 100, 15), 15);
    }

    #[test]
    fn range_params_include_step_not_time() {
        let params = range_request().to_params(100, 15);
        let keys: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["query", "start", "end", "step"]);
        assert_eq!(
            params.iter().find(|(k, _)| k == "step").unwrap().1,
            "36".to_string()
        );
    }

    #[test]
    fn instant_params_include_time_not_step() {
        let mut req = range_request();
        req.instant = true;
        let params = req.to_params(100, 15);
        let keys: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["query", "time"]);
    }

    #[test]
    fn promql_passes_through_verbatim() {
        let params = range_request().to_params(100, 15);
        assert_eq!(
            params.iter().find(|(k, _)| k == "query").unwrap().1,
            "rate(http_requests_total[5m])".to_string()
        );
    }
}
