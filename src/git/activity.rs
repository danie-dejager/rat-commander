//! A year of git activity for some paths, as a GitHub-style calendar: commits
//! per day, laid out in week columns with a row per weekday.
//!
//! Days are UTC days, the same clock every other date in the program is shown
//! on (see [`crate::util::bytes::format_time`]).

use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Week columns in a full calendar: a year, and the week in progress.
pub const WEEKS: usize = 53;

const DAY: i64 = 86_400;

/// Commits per day over the last [`WEEKS`] weeks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// Day number (days since 1970-01-01) of the Monday the first column starts.
    pub start: i64,
    /// Today's day number: the last day counted.
    pub today: i64,
    /// Commits on each day from `start` through `today`.
    pub counts: Vec<u16>,
    pub total: u32,
}

/// Weekday of a day number, Monday = 0. 1970-01-01 was a Thursday.
pub fn weekday(day: i64) -> i64 {
    (day + 3).rem_euclid(7)
}

impl Activity {
    /// An empty calendar ending on day `today`.
    pub fn empty(today: i64) -> Self {
        let start = today - weekday(today) - (WEEKS as i64 - 1) * 7;
        Activity { start, today, counts: vec![0; (today - start + 1) as usize], total: 0 }
    }

    /// Count commits made at `times` (Unix seconds); ones outside the calendar
    /// are ignored.
    pub fn from_times(times: impl IntoIterator<Item = i64>, today: i64) -> Self {
        let mut a = Activity::empty(today);
        for t in times {
            let day = t.div_euclid(DAY);
            if let Some(c) = usize::try_from(day - a.start).ok().and_then(|i| a.counts.get_mut(i)) {
                *c = c.saturating_add(1);
                a.total += 1;
            }
        }
        a
    }

    /// Commits on `day`, or `None` for a day the calendar doesn't cover (before
    /// its start, or after today).
    pub fn count(&self, day: i64) -> Option<u16> {
        usize::try_from(day - self.start).ok().and_then(|i| self.counts.get(i)).copied()
    }

    /// The busiest day's count, which the shading scales to.
    pub fn max(&self) -> u16 {
        self.counts.iter().copied().max().unwrap_or(0)
    }
}

/// Shade level 0–4 for a day with `n` commits when the busiest had `max`:
/// 0 is none, and the rest split the range into quarters.
pub fn level(n: u16, max: u16) -> usize {
    if n == 0 || max == 0 { 0 } else { (4 * n as usize).div_ceil(max as usize).clamp(1, 4) }
}

/// Today's day number.
pub fn today() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
        .div_euclid(DAY)
}

/// The activity of `names` — entries of `dir` — over the calendar ending today:
/// every commit on the current branch that touched them. `None` when `dir` is
/// not in a work tree or git can't be run.
pub async fn activity(dir: &Path, names: &[String], today: i64) -> Option<Activity> {
    let since = Activity::empty(today).start * DAY;
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("log")
        .arg(format!("--since=@{since}"))
        .arg("--format=%at")
        .arg("--")
        .args(names)
        // A file named `*.rs` is a file, not a pattern.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(Activity::from_times(text.lines().filter_map(|l| l.trim().parse().ok()), today))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-12, a Saturday.
    const TODAY: i64 = 20_708;

    #[test]
    fn weekdays_are_counted_from_monday() {
        assert_eq!(weekday(0), 3, "1970-01-01 was a Thursday");
        assert_eq!(weekday(TODAY), 5, "2026-09-12 is a Saturday");
    }

    #[test]
    fn the_calendar_starts_on_a_monday_a_year_back_and_ends_today() {
        let a = Activity::empty(TODAY);
        assert_eq!(weekday(a.start), 0);
        assert_eq!(a.today - a.start, 52 * 7 + 5, "52 full weeks and this one up to Saturday");
        assert_eq!(a.count(TODAY + 1), None, "tomorrow is not counted");
        assert_eq!(a.count(a.start - 1), None);
    }

    #[test]
    fn commits_land_on_their_utc_day() {
        let noon = |day: i64| day * DAY + 12 * 3600;
        let times = [noon(TODAY), TODAY * DAY + DAY - 1, noon(TODAY - 3), noon(TODAY - 400)];
        let a = Activity::from_times(times, TODAY);
        assert_eq!(a.count(TODAY), Some(2), "both of today's, up to the last second");
        assert_eq!(a.count(TODAY - 3), Some(1));
        assert_eq!(a.total, 3, "one from over a year ago is left out");
        assert_eq!(a.max(), 2);
    }

    #[test]
    fn levels_scale_to_the_busiest_day() {
        assert_eq!(level(0, 8), 0);
        assert_eq!(level(1, 8), 1);
        assert_eq!(level(4, 8), 2);
        assert_eq!(level(8, 8), 4);
        assert_eq!(level(3, 0), 0);
    }

    #[tokio::test]
    async fn a_real_repository_is_counted_per_path() {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("rc_activity_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let today = super::today();
        let git = |args: &[&str], days_ago: i64| {
            let date = format!("@{} +0000", (today - days_ago) * DAY + 3600);
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if !git(&["init", "-q"], 0) {
            return; // no git here
        }
        for (i, days_ago) in [10, 3, 3].into_iter().enumerate() {
            std::fs::write(dir.join("a.txt"), format!("{i}")).unwrap();
            git(&["add", "-A"], days_ago);
            git(&["commit", "-qm", "a"], days_ago);
        }
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        git(&["add", "-A"], 1);
        git(&["commit", "-qm", "b"], 1);

        let a = activity(&dir, &["a.txt".to_string()], today).await.expect("in a repository");
        assert_eq!((a.total, a.count(today - 3), a.count(today - 10)), (3, Some(2), Some(1)));
        let both = activity(&dir, &["a.txt".into(), "b.txt".into()], today).await.unwrap();
        assert_eq!(both.total, 4);

        let outside = std::env::temp_dir();
        assert!(activity(&outside, &["x".to_string()], today).await.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
