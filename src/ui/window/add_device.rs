use super::{App, Message};
use crate::devices::enums::DeviceData;
use crate::ui::style;
use bluer::Address;
use iced::border::Radius;
use iced::overlay::menu;
use iced::widget::button::Style;
use iced::widget::{
    Column, Container, Space, button, column, combo_box, container, row, scrollable, text,
    text_input,
};
use iced::{Background, Border, Center, Element, Length, Theme};
use std::collections::HashMap;

impl App {
    pub(super) fn add_device_page(
        &self,
        devices_list: &HashMap<String, DeviceData>,
    ) -> Container<'_, Message> {
        container(column![
            text("Pick a paired device to add:").size(18),
            Space::new().height(Length::from(10)),
            self.paired_device_list(devices_list)
        ])
        .padding(20)
        .height(Length::Fill)
        .width(Length::Fill)
    }

    fn paired_device_list(
        &self,
        devices_list: &HashMap<String, DeviceData>,
    ) -> scrollable::Scrollable<'_, Message> {
        let mut list_col = column![].spacing(12);
        for (name, address) in &self.paired_devices {
            if !devices_list.contains_key(&address.to_string()) {
                list_col = list_col.push(
                    container(self.paired_device_item(name, *address))
                        .padding(8)
                        .style(|theme: &Theme| style::outlined_panel(theme, 8.0)),
                );
            }
        }
        if self
            .paired_devices
            .iter()
            .all(|device| devices_list.contains_key(&device.1.to_string()))
            && self.pending_add_device.is_none()
        {
            list_col = list_col.push(
                container(
                    text("No new paired devices found. All paired devices are already added.")
                        .size(16),
                )
                .width(Length::Fill),
            );
        }
        scrollable(list_col)
            .height(Length::Fill)
            .width(Length::Fill)
    }

    fn paired_device_item(&self, name: &str, address: Address) -> Column<'_, Message> {
        let is_pending = matches!(&self.pending_add_device, Some((_, addr)) if addr == &address);
        let mut row_elements: Vec<Element<'_, Message>> = vec![
            column![
                text(name.to_string()).size(16),
                text(address.to_string()).size(12)
            ]
            .into(),
            Space::new().height(Length::Fill).into(),
        ];
        if !is_pending {
            row_elements.push(
                button(
                    text("Add")
                        .size(14)
                        .width(120)
                        .align_y(Center)
                        .align_x(Center),
                )
                .style(|theme: &Theme, _status| Style {
                    text_color: theme.palette().text,
                    background: Some(Background::Color(theme.palette().primary.scale_alpha(0.5))),
                    border: Border {
                        width: 1.0,
                        color: theme.palette().primary,
                        radius: Radius::from(8.0),
                    },
                    ..Default::default()
                })
                .padding(8)
                .on_press(Message::StartAddDevice(name.to_string(), address))
                .into(),
            );
        }
        let mut item_col = column![row(row_elements).align_y(Center)].spacing(8);
        if is_pending {
            item_col = item_col
                .push(self.device_type_picker())
                .push(confirm_buttons());
        }
        item_col
    }

    fn device_type_picker(&self) -> iced::widget::Row<'_, Message> {
        row![
            text("Device Type:").size(16),
            Space::new().width(Length::Fill),
            combo_box(
                &self.device_type_state,
                "Select device type",
                self.selected_device_type.as_ref(),
                Message::SelectDeviceType
            )
            .input_style(|theme: &Theme, _status| text_input::Style {
                background: Background::Color(theme.palette().background),
                border: Border {
                    width: 1.0,
                    color: theme.palette().text,
                    radius: Radius::from(8.0),
                },
                icon: Default::default(),
                placeholder: theme.palette().text.scale_alpha(0.5),
                value: theme.palette().text,
                selection: theme.palette().primary
            })
            .menu_style(|theme: &Theme| menu::Style {
                background: Background::Color(theme.palette().background),
                border: Border {
                    width: 1.0,
                    color: theme.palette().text,
                    radius: Radius::from(8.0)
                },
                text_color: theme.palette().text,
                selected_text_color: theme.palette().text,
                selected_background: Background::Color(theme.palette().primary.scale_alpha(0.3)),
                shadow: Default::default()
            })
            .width(Length::from(200))
        ]
    }
}

fn confirm_buttons<'a>() -> iced::widget::Row<'a, Message> {
    row![
        Space::new().width(Length::Fill),
        dialog_button("Cancel", Message::CancelAddDevice, 0.1),
        Space::new().width(Length::from(20)),
        dialog_button("Add Device", Message::ConfirmAddDevice, 0.3),
    ]
    .align_y(Center)
    .width(Length::Fill)
}

fn dialog_button(label: &str, on_press: Message, alpha: f32) -> button::Button<'_, Message> {
    button(text(label).size(16).width(Length::Fill).center())
        .on_press(on_press)
        .style(move |theme: &Theme, _status| Style {
            background: Some(Background::Color(
                theme.palette().primary.scale_alpha(alpha),
            )),
            text_color: theme.palette().text,
            border: Border::default().rounded(8.0),
            ..Default::default()
        })
        .width(Length::from(120))
        .padding(4)
}
