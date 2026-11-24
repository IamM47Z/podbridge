//! Low-battery alerts for the buds, like an iPhone's: once at 20% and once at
//! 10% per discharge. The case is left out; it does not play audio.

use crate::bluetooth::aacp::{BatteryComponent, BatteryInfo, BatteryStatus};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

const THRESHOLDS: [u8; 2] = [20, 10];
const RESET_MARGIN: u8 = 10;

/// The lowest threshold already alerted for each bud, per discharge.
#[derive(Debug, Default)]
pub struct LowBatteryAlerts {
    alerted: Vec<(BatteryComponent, u8)>,
}

impl LowBatteryAlerts {
    /// The buds that just crossed a threshold, with their levels.
    pub fn check(&mut self, battery: &[BatteryInfo]) -> Vec<(BatteryComponent, u8)> {
        let mut crossed = Vec::new();
        for b in battery {
            if b.component == BatteryComponent::Case || b.status == BatteryStatus::Disconnected {
                continue;
            }
            let last = self.alerted.iter().position(|(c, _)| *c == b.component);
            // Charging, or well above every threshold, starts a new discharge.
            // The margin keeps a level flickering at 20/21% from re-alerting.
            if b.status == BatteryStatus::Charging || b.level > THRESHOLDS[0] + RESET_MARGIN {
                if let Some(i) = last {
                    self.alerted.remove(i);
                }
                continue;
            }
            let Some(&threshold) = THRESHOLDS.iter().rev().find(|&&t| b.level <= t) else {
                continue;
            };
            match last {
                Some(i) if self.alerted[i].1 <= threshold => {}
                Some(i) => {
                    self.alerted[i].1 = threshold;
                    crossed.push((b.component, b.level));
                }
                None => {
                    self.alerted.push((b.component, threshold));
                    crossed.push((b.component, b.level));
                }
            }
        }
        crossed
    }
}

// Each reconnect creates a fresh event loop. Keep the alert state outside it,
// keyed by the AirPods' address, so a discharge alerts once per threshold
// however often the AirPods reconnect.
static ALERTS: LazyLock<Mutex<HashMap<String, LowBatteryAlerts>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// LowBatteryAlerts::check, with the state kept per AirPods across reconnects.
pub fn check_for(airpods_mac: &str, battery: &[BatteryInfo]) -> Vec<(BatteryComponent, u8)> {
    ALERTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(airpods_mac.to_ascii_uppercase())
        .or_default()
        .check(battery)
}

/// The notification text for the buds that crossed a threshold.
pub fn alert_body(crossed: &[(BatteryComponent, u8)]) -> String {
    crossed
        .iter()
        .map(|(c, level)| format!("{}: {level}%", c.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Show a desktop notification. Blocking: call it off the async runtime.
pub fn notify(summary: &str, body: &str) {
    use dbus::arg::{PropMap, Variant};
    let result = dbus::blocking::Connection::new_session().and_then(|conn| {
        let proxy = conn.with_proxy(
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            std::time::Duration::from_secs(5),
        );
        let mut hints = PropMap::new();
        hints.insert("urgency".to_string(), Variant(Box::new(1u8)));
        let actions: Vec<&str> = Vec::new();
        proxy
            .method_call::<(u32,), _, _, _>(
                "org.freedesktop.Notifications",
                "Notify",
                (
                    "podbridge",
                    0u32,
                    crate::ui::window::APP_ID,
                    summary,
                    body,
                    actions,
                    hints,
                    -1i32,
                ),
            )
            .map(|_| ())
    });
    if let Err(e) = result {
        log::warn!("Failed to show the notification \"{}\": {}", summary, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bud(component: BatteryComponent, level: u8, status: BatteryStatus) -> BatteryInfo {
        BatteryInfo {
            component,
            level,
            status,
        }
    }

    fn left(level: u8) -> BatteryInfo {
        bud(BatteryComponent::Left, level, BatteryStatus::NotCharging)
    }

    #[test]
    fn alerts_once_at_each_threshold() {
        let mut a = LowBatteryAlerts::default();
        assert!(a.check(&[left(25)]).is_empty());
        assert_eq!(a.check(&[left(20)]), [(BatteryComponent::Left, 20)]);
        assert!(a.check(&[left(15)]).is_empty());
        assert_eq!(a.check(&[left(9)]), [(BatteryComponent::Left, 9)]);
        assert!(a.check(&[left(5)]).is_empty());
    }

    #[test]
    fn first_report_below_ten_alerts_once() {
        let mut a = LowBatteryAlerts::default();
        assert_eq!(a.check(&[left(8)]), [(BatteryComponent::Left, 8)]);
        assert!(a.check(&[left(20)]).is_empty());
    }

    #[test]
    fn charging_starts_a_new_discharge() {
        let mut a = LowBatteryAlerts::default();
        a.check(&[left(18)]);
        a.check(&[bud(BatteryComponent::Left, 18, BatteryStatus::Charging)]);
        assert_eq!(a.check(&[left(18)]), [(BatteryComponent::Left, 18)]);
    }

    #[test]
    fn flicker_at_twenty_alerts_once_but_a_real_rise_resets() {
        let mut a = LowBatteryAlerts::default();
        a.check(&[left(20)]);
        assert!(a.check(&[left(21)]).is_empty());
        assert!(a.check(&[left(20)]).is_empty());
        a.check(&[left(31)]);
        assert_eq!(a.check(&[left(18)]), [(BatteryComponent::Left, 18)]);
    }

    #[test]
    fn only_the_discharging_bud_alerts() {
        let mut a = LowBatteryAlerts::default();
        let reports = [
            left(15),
            bud(BatteryComponent::Right, 15, BatteryStatus::Charging),
        ];
        assert_eq!(a.check(&reports), [(BatteryComponent::Left, 15)]);
    }

    #[test]
    fn single_unit_headphones_alert_as_airpods() {
        let mut a = LowBatteryAlerts::default();
        let crossed = a.check(&[bud(
            BatteryComponent::Headphone,
            12,
            BatteryStatus::NotCharging,
        )]);
        assert_eq!(alert_body(&crossed), "AirPods: 12%");
    }

    #[test]
    fn case_and_disconnected_buds_never_alert() {
        let mut a = LowBatteryAlerts::default();
        let reports = [
            bud(BatteryComponent::Case, 5, BatteryStatus::NotCharging),
            bud(BatteryComponent::Right, 5, BatteryStatus::Disconnected),
        ];
        assert!(a.check(&reports).is_empty());
    }

    #[test]
    fn alerts_once_per_airpods_across_reconnects() {
        // Made-up addresses, unique to this test: the state is process-wide.
        const A: &str = "02:00:00:00:00:A1";
        const B: &str = "02:00:00:00:00:B1";
        assert_eq!(check_for(A, &[left(19)]), [(BatteryComponent::Left, 19)]);
        // A reconnect of the same AirPods does not alert again.
        assert!(check_for(A, &[left(19)]).is_empty());
        // Other AirPods keep their own state.
        assert_eq!(check_for(B, &[left(19)]), [(BatteryComponent::Left, 19)]);
    }

    #[test]
    fn body_names_each_bud() {
        let body = alert_body(&[(BatteryComponent::Left, 19), (BatteryComponent::Right, 20)]);
        assert_eq!(body, "Left: 19%, Right: 20%");
    }
}
