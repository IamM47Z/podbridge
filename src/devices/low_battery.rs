//! Low-battery alerts for the buds, like an iPhone's: once at 20% and once at
//! 10% per discharge. The case is left out; it does not play audio.

use crate::bluetooth::aacp::{BatteryComponent, BatteryInfo, BatteryStatus};

const THRESHOLDS: [u8; 2] = [20, 10];

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
            // Charging, or back above every threshold, starts a new discharge.
            if b.status == BatteryStatus::Charging || b.level > THRESHOLDS[0] {
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

/// The notification text for the buds that crossed a threshold.
pub fn alert_body(crossed: &[(BatteryComponent, u8)]) -> String {
    crossed
        .iter()
        .map(|(c, level)| {
            let name = match c {
                BatteryComponent::Left => "Left",
                BatteryComponent::Right => "Right",
                _ => "AirPods",
            };
            format!("{name}: {level}%")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Show a desktop notification. Blocking: call it off the async runtime.
pub fn notify(body: &str) {
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
                    "AirPods battery low",
                    body,
                    actions,
                    hints,
                    -1i32,
                ),
            )
            .map(|_| ())
    });
    if let Err(e) = result {
        log::warn!("Failed to show the low-battery notification: {}", e);
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
    fn case_and_disconnected_buds_never_alert() {
        let mut a = LowBatteryAlerts::default();
        let reports = [
            bud(BatteryComponent::Case, 5, BatteryStatus::NotCharging),
            bud(BatteryComponent::Right, 5, BatteryStatus::Disconnected),
        ];
        assert!(a.check(&reports).is_empty());
    }

    #[test]
    fn body_names_each_bud() {
        let body = alert_body(&[(BatteryComponent::Left, 19), (BatteryComponent::Right, 20)]);
        assert_eq!(body, "Left: 19%, Right: 20%");
    }
}
