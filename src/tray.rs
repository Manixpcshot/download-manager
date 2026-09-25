use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    PauseAll,
    ResumeAll,
    Exit,
}

pub struct TrayController {
    // Keeping the icon alive is what keeps the native tray entry registered.
    _icon: TrayIcon,
    open_id: tray_icon::menu::MenuId,
    pause_all_id: tray_icon::menu::MenuId,
    resume_all_id: tray_icon::menu::MenuId,
    exit_id: tray_icon::menu::MenuId,
}

impl TrayController {
    pub fn new() -> Option<Self> {
        let open = MenuItem::new("Open", true, None);
        let pause_all = MenuItem::new("Pause All", true, None);
        let resume_all = MenuItem::new("Resume All", true, None);
        let exit = MenuItem::new("Exit", true, None);
        let menu = Menu::with_items(&[&open, &pause_all, &resume_all, &exit]).ok()?;
        let icon = Icon::from_rgba(tray_pixels(), 16, 16).ok()?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Pulse Download Manager")
            .with_icon(icon)
            .build()
            .ok()?;
        Some(Self {
            _icon: icon,
            open_id: open.id().clone(),
            pause_all_id: pause_all.id().clone(),
            resume_all_id: resume_all.id().clone(),
            exit_id: exit.id().clone(),
        })
    }

    pub fn actions(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.open_id {
                actions.push(TrayAction::Open);
            } else if event.id == self.pause_all_id {
                actions.push(TrayAction::PauseAll);
            } else if event.id == self.resume_all_id {
                actions.push(TrayAction::ResumeAll);
            } else if event.id == self.exit_id {
                actions.push(TrayAction::Exit);
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click { button, .. } = event {
                if button == MouseButton::Left {
                    actions.push(TrayAction::Open);
                }
            }
        }
        actions
    }
}

fn tray_pixels() -> Vec<u8> {
    let mut pixels = vec![0_u8; 16 * 16 * 4];
    for y in 0..16 {
        for x in 0..16 {
            let distance = ((x as i32 - 8).pow(2) + (y as i32 - 8).pow(2)) as f32;
            let index = (y * 16 + x) * 4;
            if distance < 52.0 {
                pixels[index] = 124;
                pixels[index + 1] = 108;
                pixels[index + 2] = 255;
                pixels[index + 3] = 255;
            } else if distance < 64.0 {
                pixels[index] = 124;
                pixels[index + 1] = 108;
                pixels[index + 2] = 255;
                pixels[index + 3] = 120;
            }
        }
    }
    pixels
}
