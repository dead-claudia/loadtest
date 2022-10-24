use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

#[derive(Debug)]
pub struct TcpServerStats {
    conns: AtomicUsize,
    recv: AtomicUsize,
}

impl TcpServerStats {
    pub fn new() -> TcpServerStats {
        TcpServerStats {
            conns: AtomicUsize::new(0),
            recv: AtomicUsize::new(0),
        }
    }

    pub fn add_connection(&self) {
        self.conns.fetch_add(1, Ordering::AcqRel);
    }

    pub fn remove_connection(&self) {
        self.conns.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn push_received_bytes(&self, bytes: usize) {
        self.recv.fetch_add(bytes, Ordering::AcqRel);
    }

    pub fn get_snapshot(&self, duration: Duration) -> Option<String> {
        let duration_ms = (duration.as_nanos() as f64) / 1_000_000.0;

        // Skip negative durations and not-positive-enough durations.
        if duration_ms < 1.0 {
            return None;
        }

        let recv_found = self.recv.swap(0, Ordering::AcqRel);
        let conns = self.conns.load(Ordering::Acquire);

        let recv_kbps = recv_found as f64 * 8.0 / duration_ms;

        let mut builder = String::new();

        fn push_formatted(builder: &mut String, num: usize) {
            let s = num.to_string();

            if s.len() <= 3 {
                builder.push_str(s.as_str());
            } else {
                let chunk_count = s.len() / 3;
                let remainder = s.len() % 3;
                let (kbps_start, kbps_part) = s.split_at(remainder);
                builder.push_str(kbps_start);
                for i in 0..chunk_count {
                    builder.push(' ');
                    builder.push_str(&kbps_part[i * 3..i * 3 + 3]);
                }
            }
        }

        push_formatted(&mut builder, conns);
        builder.push_str(" conns, ");
        push_formatted(&mut builder, recv_kbps as usize);
        builder.push_str(" Kb/s in");

        Some(builder)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn server_stats_skips_empty_snapshot_if_zero_time() {
        let stats = TcpServerStats::new();
        let snapshot = stats.get_snapshot(Duration::from_secs(0));
        assert_matches!(snapshot, None)
    }

    #[test]
    fn server_stats_skips_empty_snapshot_if_almost_zero_time() {
        let stats = TcpServerStats::new();
        let snapshot = stats.get_snapshot(Duration::from_micros(100));
        assert_matches!(snapshot, None)
    }

    #[test]
    fn server_stats_renders_empty_snapshot_correctly() {
        let stats = TcpServerStats::new();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
    }

    #[test]
    fn server_stats_renders_snapshot_with_one_initial_conn_correctly() {
        let stats = TcpServerStats::new();
        stats.add_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 0 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 0 Kb/s in");
        stats.remove_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
    }

    #[test]
    fn server_stats_renders_snapshot_with_one_not_initial_conn_correctly() {
        let stats = TcpServerStats::new();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
        stats.add_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 0 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 0 Kb/s in");
        stats.remove_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
    }

    #[test]
    fn server_stats_renders_snapshot_with_multiple_not_initial_conn_correctly() {
        let stats = TcpServerStats::new();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
        stats.add_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 0 Kb/s in");
        stats.add_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "2 conns, 0 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "2 conns, 0 Kb/s in");
        stats.remove_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 0 Kb/s in");
        stats.remove_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
    }

    #[test]
    fn server_stats_renders_snapshot_with_one_initial_conn_and_some_bytes_correctly() {
        let stats = TcpServerStats::new();
        stats.add_connection();
        stats.push_received_bytes(15432);
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 123 Kb/s in");
        stats.push_received_bytes(124875);
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 999 Kb/s in");
        stats.push_received_bytes(1520);
        stats.remove_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 12 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
    }

    #[test]
    fn server_stats_renders_snapshot_with_one_not_initial_conn_and_some_bytes_correctly() {
        let stats = TcpServerStats::new();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
        stats.add_connection();
        stats.push_received_bytes(15432);
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 123 Kb/s in");
        stats.push_received_bytes(124875);
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 999 Kb/s in");
        stats.push_received_bytes(1520);
        stats.remove_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 12 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
    }

    #[test]
    fn server_stats_renders_snapshot_with_multiple_not_initial_conn_and_some_bytes_correctly() {
        let stats = TcpServerStats::new();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
        stats.add_connection();
        stats.push_received_bytes(15432);
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 123 Kb/s in");
        stats.add_connection();
        stats.push_received_bytes(124875);
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "2 conns, 999 Kb/s in");
        stats.push_received_bytes(555555);
        stats.push_received_bytes(555555);
        let snapshot = stats.get_snapshot(Duration::from_secs(2)).unwrap();
        assert_eq!(snapshot, "2 conns, 4 444 Kb/s in");
        stats.push_received_bytes(123456789);
        stats.push_received_bytes(123456789);
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "2 conns, 1 975 308 Kb/s in");
        stats.push_received_bytes(100);
        stats.remove_connection();
        stats.push_received_bytes(100);
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "1 conns, 1 Kb/s in");
        stats.push_received_bytes(1520);
        stats.remove_connection();
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 12 Kb/s in");
        let snapshot = stats.get_snapshot(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot, "0 conns, 0 Kb/s in");
    }
}
