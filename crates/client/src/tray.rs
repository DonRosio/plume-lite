//! The icon in the notification area. Closing the window keeps Plume Lite running there (for
//! the notifications); a click on the icon brings it back, its menu quits. A pink dot on the
//! icon means something is unread.
//!
//! Linux: a StatusNotifierItem over D-Bus (ksni, pure Rust), shown by KDE, XFCE, Cinnamon,
//! MATE, and GNOME with its AppIndicator extension. Windows: the system tray (tray-icon).
//! Without a notification area, closing the window quits, as before.

use std::sync::{Mutex, OnceLock};

use iced::Subscription;
use iced::futures::{Stream, stream};
use tokio::sync::mpsc;

/// What the icon's menu asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Show,
    ToggleDoNotDisturb,
    Quit,
}

/// What the icon shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrayState {
    pub unread: bool,
    pub do_not_disturb: bool,
}

type Channel = (mpsc::UnboundedSender<TrayEvent>, Mutex<Option<mpsc::UnboundedReceiver<TrayEvent>>>);

fn channel() -> &'static Channel {
    static CHANNEL: OnceLock<Channel> = OnceLock::new();
    CHANNEL.get_or_init(|| {
        let (sender, receiver) = mpsc::unbounded_channel();
        (sender, Mutex::new(Some(receiver)))
    })
}

fn send(event: TrayEvent) {
    let _ = channel().0.send(event);
}

/// The menu's clicks, for the application.
pub fn events() -> Subscription<TrayEvent> {
    Subscription::run(stream)
}

fn stream() -> impl Stream<Item = TrayEvent> {
    let receiver = channel().1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    stream::unfold(receiver, |receiver| async move {
        let mut receiver = receiver?;
        let event = receiver.recv().await?;
        Some((event, Some(receiver)))
    })
}

pub struct Tray {
    state: TrayState,
    inner: platform::Tray,
}

impl Tray {
    /// `None` when this desktop has no notification area.
    pub fn start() -> Option<Self> {
        let state = TrayState::default();
        match platform::Tray::start(state) {
            Ok(inner) => Some(Self { state, inner }),
            Err(err) => {
                eprintln!("no notification area: {err}");
                None
            }
        }
    }

    pub fn set_state(&mut self, state: TrayState) {
        if state != self.state {
            self.state = state;
            self.inner.set_state(state);
        }
    }
}

// --- The icon, from assets/plume-lite.ico ----------------------------------------------------

const ICO: &[u8] = include_bytes!("../assets/plume-lite.ico");

/// The `size`×`size` image of the icon file (its entries are PNG), as RGBA, with a pink dot
/// in the corner when something is unread.
fn icon(size: u32, unread: bool) -> Option<Vec<u8>> {
    let count = u16::from_le_bytes(ICO.get(4..6)?.try_into().ok()?) as usize;
    let entry = (0..count).map(|i| 6 + 16 * i).find(|&at| {
        let width = ICO.get(at).copied().unwrap_or_default();
        u32::from(width) == size % 256
    })?;
    let length = u32::from_le_bytes(ICO.get(entry + 8..entry + 12)?.try_into().ok()?) as usize;
    let offset = u32::from_le_bytes(ICO.get(entry + 12..entry + 16)?.try_into().ok()?) as usize;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(ICO.get(offset..offset + length)?));
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut rgba).ok()?;
    if (frame.width, frame.height, frame.color_type) != (size, size, png::ColorType::Rgba) {
        return None;
    }
    rgba.truncate(frame.buffer_size());
    if unread {
        dot(&mut rgba, size);
    }
    Some(rgba)
}

/// A pink dot ringed with white, in the bottom-right corner.
fn dot(rgba: &mut [u8], size: u32) {
    let pink = crate::theme::LIGHT.pink;
    let (radius, ring) = (size as f32 * 0.22, size as f32 * 0.06);
    let center = size as f32 - radius - ring;
    for y in 0..size {
        for x in 0..size {
            let distance = ((x as f32 + 0.5 - center).powi(2) + (y as f32 + 0.5 - center).powi(2)).sqrt();
            let color = if distance <= radius {
                [pink.r, pink.g, pink.b]
            } else if distance <= radius + ring {
                [1.0, 1.0, 1.0]
            } else {
                continue;
            };
            let at = ((y * size + x) * 4) as usize;
            for (channel, value) in rgba[at..at + 3].iter_mut().zip(color) {
                *channel = (value * 255.0).round() as u8;
            }
            rgba[at + 3] = 255;
        }
    }
}

// --- Linux ------------------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod platform {
    use ksni::blocking::{Handle, TrayMethods};
    use ksni::menu::{CheckmarkItem, StandardItem};

    use super::{TrayEvent, TrayState, icon, send};

    pub struct Tray(Handle<Item>);

    pub struct Item {
        state: TrayState,
    }

    impl Tray {
        pub fn start(state: TrayState) -> Result<Self, String> {
            Item { state }.spawn().map(Tray).map_err(|err| err.to_string())
        }

        pub fn set_state(&mut self, state: TrayState) {
            self.0.update(|item| item.state = state);
        }
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "plume-lite".into()
        }

        fn title(&self) -> String {
            "Plume Lite".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [32, 64]
                .into_iter()
                .filter_map(|size| {
                    let rgba = icon(size, self.state.unread)?;
                    // StatusNotifierItem wants ARGB, most significant byte first.
                    let data = rgba.as_chunks::<4>().0.iter().flat_map(|&[r, g, b, a]| [a, r, g, b]).collect();
                    Some(ksni::Icon { width: size as i32, height: size as i32, data })
                })
                .collect()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            let description = if self.state.unread { "Messages non lus" } else { "" };
            ksni::ToolTip { title: "Plume Lite".into(), description: description.into(), ..Default::default() }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            send(TrayEvent::Show);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            vec![
                StandardItem {
                    label: "Ouvrir Plume Lite".into(),
                    activate: Box::new(|_: &mut Self| send(TrayEvent::Show)),
                    ..Default::default()
                }
                .into(),
                CheckmarkItem {
                    label: "Ne pas déranger".into(),
                    checked: self.state.do_not_disturb,
                    activate: Box::new(|_: &mut Self| send(TrayEvent::ToggleDoNotDisturb)),
                    ..Default::default()
                }
                .into(),
                ksni::MenuItem::Separator,
                StandardItem {
                    label: "Quitter".into(),
                    activate: Box::new(|_: &mut Self| send(TrayEvent::Quit)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }
}

// --- Windows ----------------------------------------------------------------------------------

#[cfg(windows)]
mod platform {
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::{TrayEvent, TrayState, icon, send};

    const SIZE: u32 = 32;

    /// Created on the window's thread, whose message loop (winit's) serves it.
    pub struct Tray {
        icon: TrayIcon,
        do_not_disturb: CheckMenuItem,
    }

    impl Tray {
        pub fn start(state: TrayState) -> Result<Self, String> {
            let show = MenuItem::new("Ouvrir Plume Lite", true, None);
            let do_not_disturb = CheckMenuItem::new("Ne pas déranger", true, state.do_not_disturb, None);
            let quit = MenuItem::new("Quitter", true, None);
            let menu = Menu::new();
            menu.append_items(&[&show, &do_not_disturb, &PredefinedMenuItem::separator(), &quit])
                .map_err(|err| err.to_string())?;
            let ids = (show.id().clone(), do_not_disturb.id().clone(), quit.id().clone());
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if event.id == ids.0 {
                    send(TrayEvent::Show);
                } else if event.id == ids.1 {
                    send(TrayEvent::ToggleDoNotDisturb);
                } else if event.id == ids.2 {
                    send(TrayEvent::Quit);
                }
            }));
            TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } =
                    event
                {
                    send(TrayEvent::Show);
                }
            }));
            let icon = TrayIconBuilder::new()
                .with_tooltip("Plume Lite")
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_icon(image(state)?)
                .build()
                .map_err(|err| err.to_string())?;
            Ok(Self { icon, do_not_disturb })
        }

        pub fn set_state(&mut self, state: TrayState) {
            if let Ok(image) = image(state) {
                let _ = self.icon.set_icon(Some(image));
            }
            let _ = self.icon.set_tooltip(Some(if state.unread {
                "Plume Lite — messages non lus"
            } else {
                "Plume Lite"
            }));
            self.do_not_disturb.set_checked(state.do_not_disturb);
        }
    }

    fn image(state: TrayState) -> Result<Icon, String> {
        let rgba = icon(SIZE, state.unread).ok_or("the icon file has no 32×32 image")?;
        Icon::from_rgba(rgba, SIZE, SIZE).map_err(|err| err.to_string())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::TrayState;

    pub struct Tray;

    impl Tray {
        pub fn start(_: TrayState) -> Result<Self, String> {
            Err("not supported on this system".into())
        }

        pub fn set_state(&mut self, _: TrayState) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_comes_from_the_icon_file() {
        for size in [32, 64] {
            let plain = icon(size, false).unwrap();
            assert_eq!(plain.len(), (size * size * 4) as usize);
            let marked = icon(size, true).unwrap();
            assert_ne!(plain, marked, "the dot");
            let corner = (((size - 4) * size + size - 9) * 4) as usize;
            assert_eq!(marked[corner + 3], 255, "opaque dot in the corner");
        }
        assert!(icon(20, false).is_none(), "no such size");
    }
}
