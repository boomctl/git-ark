pub trait Clock {
    fn timestamp(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn timestamp(&self) -> String {
        use time::format_description::FormatItem;
        use time::macros::format_description;
        const FMT: &[FormatItem] =
            format_description!("[year]-[month]-[day]T[hour]-[minute]-[second]Z");
        time::OffsetDateTime::now_utc()
            .format(&FMT)
            .expect("format utc timestamp")
    }
}

pub struct FixedClock(pub String);

impl Clock for FixedClock {
    fn timestamp(&self) -> String {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_clock_returns_its_value() {
        assert_eq!(FixedClock("2026-01-02T03-04-05Z".into()).timestamp(), "2026-01-02T03-04-05Z");
    }
    #[test]
    fn system_clock_is_safe_and_zulu() {
        let t = SystemClock.timestamp();
        assert!(t.ends_with('Z'));
        assert!(!t.contains(':'), "colons are avoided for key/filename safety");
    }
}
