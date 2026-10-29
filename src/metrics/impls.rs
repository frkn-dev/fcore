use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use sysinfo::Disks;
use sysinfo::Networks;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

use super::storage::HasMetrics;
use super::storage::MetricSink;
use super::Metrics;

static BEAT_INDEX: OnceLock<AtomicUsize> = OnceLock::new();

/// Фиксирует старт процесса — вызывается в main, чтобы аптайм не начинался
/// с первого обращения к /healthcheck.
pub fn mark_process_start() {
    START.get_or_init(std::time::Instant::now);
}

/// Сколько секунд работает процесс (для /healthcheck).
pub fn process_uptime_secs() -> f64 {
    let started = *START.get_or_init(std::time::Instant::now);
    started.elapsed().as_secs_f64()
}

/// Момент старта процесса — чтобы api видел перезапуски ноды.
static START: OnceLock<std::time::Instant> = OnceLock::new();

impl<T> Metrics for T
where
    T: HasMetrics + Sync + Send,
{
    async fn heartbeat(&self) {
        let started = *START.get_or_init(std::time::Instant::now);
        let pattern: &[f64] = &[
            1.0, 1.0, 2.5, 0.4, 0.6, 1.3, 1.0, 1.0, 1.1, 1.0, 1.0, 0.8, 1.0, 1.9, 0.3, 0.5, 1.0,
        ];
        let value = {
            let index = BEAT_INDEX
                .get_or_init(|| AtomicUsize::new(0))
                .fetch_add(1, Ordering::Relaxed)
                % pattern.len();
            pattern[index]
        };

        let node = self.node_settings();
        let tags = node.get_base_tags();
        let node_uuid = node.uuid;

        self.metrics()
            .write(&node_uuid, "sys.heartbeat", value, tags.clone());
        // Аптайм процесса: если значение уменьшилось — нода перезапускалась.
        self.metrics().write(
            &node_uuid,
            "sys.uptime",
            started.elapsed().as_secs_f64(),
            tags,
        );
    }

    async fn memory(&self) {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
        );
        system.refresh_memory();

        let free = system.free_memory() as f64;
        let total = system.total_memory() as f64;
        let used = system.used_memory() as f64;
        let available = system.available_memory() as f64;

        let node = self.node_settings();
        let tags = node.get_base_tags();
        let node_uuid = node.uuid;

        self.metrics()
            .write(&node_uuid, "sys.mem_free", free, tags.clone());
        self.metrics()
            .write(&node_uuid, "sys.mem_total", total, tags.clone());
        self.metrics()
            .write(&node_uuid, "sys.mem_used", used, tags.clone());
        self.metrics()
            .write(&node_uuid, "sys.mem_available", available, tags);
    }

    async fn loadavg(&self) {
        let load = System::load_average();

        let node = self.node_settings();
        let tags = node.get_base_tags();
        let node_uuid = node.uuid;

        self.metrics()
            .write(&node_uuid, "sys.loadavg_1", load.one, tags.clone());
        self.metrics()
            .write(&node_uuid, "sys.loadavg_5", load.five, tags.clone());
        self.metrics()
            .write(&node_uuid, "sys.loadavg_15", load.fifteen, tags);
    }

    async fn bandwidth(&self) {
        let mut networks = Networks::new_with_refreshed_list();
        std::thread::sleep(std::time::Duration::from_secs(1));
        networks.refresh(true);

        let default_if = self.node_settings().interface.clone();

        let node = self.node_settings();
        let mut tags = node.get_base_tags();
        let node_uuid = node.uuid;

        for (interface, data) in networks.iter() {
            if interface == &default_if {
                tags.insert("default_interface".to_string(), "true".to_string());
            } else {
                tags.insert("default_interface".to_string(), "false".to_string());
            }

            self.metrics().write(
                &node_uuid,
                &format!("net.{interface}.total_rx_bps"),
                data.total_received() as f64,
                tags.clone(),
            );

            self.metrics().write(
                &node_uuid,
                &format!("net.{interface}.total_tx_bps"),
                data.total_transmitted() as f64,
                tags.clone(),
            );

            self.metrics().write(
                &node_uuid,
                &format!("net.{interface}.rx_bps"),
                data.received() as f64,
                tags.clone(),
            );
            self.metrics().write(
                &node_uuid,
                &format!("net.{interface}.tx_bps"),
                data.transmitted() as f64,
                tags.clone(),
            );
        }
    }
    async fn cpu_usage(&self) {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()),
        );

        sys.refresh_cpu_all();
        std::thread::sleep(std::time::Duration::from_millis(1000));
        sys.refresh_cpu_all();

        let node = self.node_settings();
        let tags = node.get_base_tags();
        let node_uuid = node.uuid;

        for cpu in sys.cpus() {
            let usage = (cpu.cpu_usage() * 100.0).round() / 100.0;
            let metric_name = format!("sys.cpu.{}", cpu.name());

            self.metrics()
                .write(&node_uuid, &metric_name, usage as f64, tags.clone());
        }
    }

    async fn disk_usage(&self) {
        let disks = Disks::new_with_refreshed_list();

        let node = self.node_settings();
        let tags = node.get_base_tags();
        let node_uuid = node.uuid;

        for disk in &disks {
            let mount_point = disk.mount_point().to_str().unwrap_or("unknown");
            let safe_mount = if mount_point == "/" {
                "root"
            } else {
                mount_point.trim_start_matches('/')
            };

            let total = disk.total_space();
            let available = disk.available_space();
            let used = total - available;

            let usage_percent = if total > 0 {
                (used as f64 / total as f64 * 100.0).round() / 100.0
            } else {
                0.0
            };

            self.metrics().write(
                &node_uuid,
                &format!("sys.disk.{}.used_bytes", safe_mount),
                used as f64,
                tags.clone(),
            );

            self.metrics().write(
                &node_uuid,
                &format!("sys.disk.{}.total_bytes", safe_mount),
                total as f64,
                tags.clone(),
            );

            self.metrics().write(
                &node_uuid,
                &format!("sys.disk.{}.usage_percent", safe_mount),
                usage_percent,
                tags.clone(),
            );
        }
    }
}
