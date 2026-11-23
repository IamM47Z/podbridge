use super::{App, Message, Tab};
use crate::bluetooth::aacp::{BatteryComponent, BatteryStatus};
use crate::devices::enums::{AirPodsState, DeviceData, DeviceState};
use crate::ui::style;
use iced::border::Radius;
use iced::widget::rule::FillMode;
use iced::widget::{Column, Space, button, column, container, pane_grid, row, rule, text};
use iced::{Center, Element, Length, Theme};
use std::collections::HashMap;

impl App {
    pub(super) fn sidebar_pane<'a>(
        &'a self,
        devices_list: &HashMap<String, DeviceData>,
    ) -> pane_grid::Content<'a, Message> {
        let content = column![
            row![
                text("Devices").size(18),
                Space::new().width(Length::Fill),
                button(
                    container(text("+").size(18))
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
                )
                .style(|theme: &Theme, _status| style::sidebar_tab(theme, false))
                .padding(0)
                .width(Length::from(28))
                .height(Length::from(28))
                .on_press(Message::ShowNewDialogTab)
            ]
            .align_y(Center)
            .padding(4),
            Space::new().height(Length::from(8)),
            self.device_buttons(devices_list),
            Space::new().height(Length::Fill),
            self.settings_button()
        ]
        .padding(12);
        pane_grid::Content::new(row![
            content,
            rule::vertical(1).style(|theme: &Theme| {
                rule::Style {
                    color: theme.palette().primary.scale_alpha(0.2),
                    radius: Radius::from(8.0),
                    fill_mode: FillMode::Full,
                    snap: false,
                }
            })
        ])
    }

    /// One button per known device, sorted by name.
    fn device_buttons<'a>(
        &'a self,
        devices_list: &HashMap<String, DeviceData>,
    ) -> Column<'a, Message> {
        let mut devices = column!().spacing(4);
        let mut devices_vec: Vec<(&String, &DeviceData)> = devices_list.iter().collect();
        devices_vec.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        for (mac, device) in devices_vec {
            let tab_button = self.device_tab_button(
                Tab::Device(mac.clone()),
                &device.name,
                mac,
                self.bluetooth_state.connected_devices.contains(mac),
            );
            devices = devices.push(tab_button);
        }
        devices
    }

    fn device_tab_button<'a>(
        &'a self,
        tab: Tab,
        label: &str,
        mac_addr: &str,
        connected: bool,
    ) -> Element<'a, Message> {
        let label = label.to_string() + if connected { " \u{100263}" } else { "" };
        let col = column![
            text(label).size(16),
            text(self.tab_subtitle(&tab, mac_addr, connected)).size(12)
        ];
        self.tab_button(tab, col)
    }

    /// The line under a device name: battery when connected, the address otherwise.
    fn tab_subtitle(&self, tab: &Tab, mac_addr: &str, connected: bool) -> String {
        if !connected {
            return mac_addr.to_string();
        }
        let mac = match tab {
            Tab::Device(mac) => mac.as_str(),
            _ => "",
        };
        match self.device_states.get(mac) {
            Some(DeviceState::AirPods(state)) => battery_text(state),
            _ => "Connected".to_string(),
        }
    }

    fn settings_button(&self) -> Element<'_, Message> {
        self.tab_button(
            Tab::Settings,
            column![text("Settings".to_string()).size(16)],
        )
    }

    fn tab_button<'a>(&self, tab: Tab, col: Column<'a, Message>) -> Element<'a, Message> {
        let is_selected = self.selected_tab == tab;
        button(container(col).padding(8))
            .style(move |theme: &Theme, _status| style::sidebar_tab(theme, is_selected))
            .padding(5)
            .on_press(Message::SelectTab(tab))
            .width(Length::Fill)
            .into()
    }
}

const CHARGING: &str = "\u{1002E6}";

fn battery_text(state: &AirPodsState) -> String {
    let find = |component| state.battery.iter().find(|x| x.component == component);
    // if headphones is not None, use only that
    if let Some(headphone) = find(BatteryComponent::Headphone) {
        let charging = headphone.status == BatteryStatus::Charging;
        return format!(
            "\u{100EB9} {}%{}",
            headphone.level,
            if charging { CHARGING } else { "" }
        );
    }
    let level = |component| find(component).map(|x| x.level).unwrap_or_default();
    let charging = |component| {
        if find(component).is_some_and(|x| x.status == BatteryStatus::Charging) {
            CHARGING
        } else {
            ""
        }
    };
    let (left, right, case) = (
        BatteryComponent::Left,
        BatteryComponent::Right,
        BatteryComponent::Case,
    );
    format!(
        "\u{1018E5} {}%{} \u{1018E8} {}%{} \u{100E6C} {}%{}",
        level(left),
        charging(left),
        level(right),
        charging(right),
        level(case),
        charging(case)
    )
}
