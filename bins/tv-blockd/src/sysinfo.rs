//! Parsers for the `/proc` and `app_launcher` text behind the manager's
//! System tab.

/// `/proc/stat`'s `cpu` line: (all ticks, idle + iowait ticks).
pub fn cpu_ticks(stat: &str) -> Option<(u64, u64)> {
    let fields: Vec<u64> = stat
        .lines()
        .next()?
        .strip_prefix("cpu ")?
        .split_whitespace()
        .filter_map(|field| field.parse().ok())
        .collect();
    let idle = fields.get(3)? + fields.get(4).unwrap_or(&0);
    Some((fields.iter().sum(), idle))
}

/// `/proc/<pid>/stat`: (comm, utime + stime ticks). The comm may hold
/// spaces and parentheses, so it ends at the last `)`.
pub fn process_ticks(stat: &str) -> Option<(String, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?.to_owned();
    let rest: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    // After the comm: state is field 3, utime 14 and stime 15.
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    Some((comm, utime + stime))
}

/// A `Key:   123 kB` line of `/proc/meminfo` or `/proc/<pid>/status`.
pub fn kb(text: &str, key: &str) -> u64 {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .and_then(|value| value.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

/// A `name value` line of `/proc/vmstat`.
pub fn vmstat(text: &str, key: &str) -> u64 {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Sectors read and written on whole disks (`mmcblkN`, `sdX`) in
/// `/proc/diskstats`; partitions would count twice.
pub fn disk_sectors(diskstats: &str) -> (u64, u64) {
    diskstats
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let name = *fields.get(2)?;
            let whole = name
                .strip_prefix("mmcblk")
                .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
                || (name.len() == 3 && name.starts_with("sd"));
            whole.then(|| {
                let read = fields.get(5).and_then(|v| v.parse().ok()).unwrap_or(0);
                let written = fields.get(9).and_then(|v| v.parse().ok()).unwrap_or(0);
                (read, written)
            })
        })
        .fold((0, 0), |(r, w), (read, written): (u64, u64)| {
            (r + read, w + written)
        })
}

/// `app_launcher -S` lines (`\t  org.tizen.hbbtv (26704)`): (app id, pid).
pub fn running_apps(text: &str) -> Vec<(String, i32)> {
    text.lines()
        .filter_map(|line| {
            let (app, pid) = line.trim().rsplit_once(" (")?;
            Some((app.to_owned(), pid.strip_suffix(')')?.parse().ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_text() {
        assert_eq!(
            cpu_ticks("cpu  100 5 50 800 40 0 5 0 0 0\ncpu0 1 2 3 4\n"),
            Some((1000, 840))
        );
        let stat = "1234 (Web Content (x)) S 1 2 3 4 5 6 7 8 9 10 70 30 0 0";
        assert_eq!(process_ticks(stat), Some(("Web Content (x)".into(), 100)));
        let status = "Name:\tacr\nVmRSS:\t   45000 kB\nVmSwap:\t  1200 kB\n";
        assert_eq!((kb(status, "VmRSS"), kb(status, "VmSwap")), (45000, 1200));
        assert_eq!(kb(status, "VmPeak"), 0);
        assert_eq!(vmstat("pswpin 10\npswpout 20\n", "pswpout"), 20);
        let disks = "179 0 mmcblk0 1 2 100 4 5 6 200 8\n179 1 mmcblk0p1 1 2 50 4 5 6 70 8\n\
                     8 0 sda 1 2 10 4 5 6 20 8\n254 0 zram0 1 2 999 4 5 6 999 8\n";
        assert_eq!(disk_sectors(disks), (110, 220));
        assert_eq!(
            running_apps("\t  com.samsung.tv.csfs (414)\n\t  org.tizen.hbbtv (26704)\njunk\n"),
            vec![
                ("com.samsung.tv.csfs".into(), 414),
                ("org.tizen.hbbtv".into(), 26704)
            ]
        );
    }
}
