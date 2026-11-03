use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

pub struct Icons {
    pub nav_switch: TextureHandle,
    pub nav_exchange: TextureHandle,
    pub nav_pointer: TextureHandle,
    pub nav_network: TextureHandle,
    pub nav_machines: TextureHandle,
    pub nav_updates: TextureHandle,
    pub nav_misc: TextureHandle,
    pub nav_layout: TextureHandle,
    pub action_save: TextureHandle,
    pub action_share: TextureHandle,
}

impl Icons {
    pub fn load(ctx: &egui::Context) -> Self {
        Self {
            nav_switch: load(ctx, "nav_switch", include_bytes!("../../assets/icons/nav_switch.png")),
            nav_exchange: load(ctx, "nav_exchange", include_bytes!("../../assets/icons/nav_exchange.png")),
            nav_pointer: load(ctx, "nav_pointer", include_bytes!("../../assets/icons/nav_pointer.png")),
            nav_network: load(ctx, "nav_network", include_bytes!("../../assets/icons/nav_network.png")),
            nav_machines: load(ctx, "nav_machines", include_bytes!("../../assets/icons/nav_machines.png")),
            nav_updates: load(ctx, "nav_updates", include_bytes!("../../assets/icons/nav_updates.png")),
            nav_misc: load(ctx, "nav_misc", include_bytes!("../../assets/icons/nav_misc.png")),
            nav_layout: load(ctx, "nav_layout", include_bytes!("../../assets/icons/nav_layout.png")),
            action_save: load(ctx, "action_save", include_bytes!("../../assets/icons/action_save.png")),
            action_share: load(ctx, "action_share", include_bytes!("../../assets/icons/action_share.png")),        }
    }

    pub fn for_tab(&self, tab: crate::gui::Tab) -> &TextureHandle {
        match tab {
            crate::gui::Tab::Switching => &self.nav_switch,
            crate::gui::Tab::Exchange => &self.nav_exchange,
            crate::gui::Tab::Output => &self.nav_pointer,
            crate::gui::Tab::Network => &self.nav_network,
            crate::gui::Tab::Clients | crate::gui::Tab::Connected => &self.nav_machines,
            crate::gui::Tab::Internet => &self.nav_updates,
            crate::gui::Tab::Misc | crate::gui::Tab::Logs => &self.nav_misc,
            crate::gui::Tab::MonitorManager => &self.nav_layout,
        }
    }
}

fn load(ctx: &egui::Context, name: &str, bytes: &[u8]) -> TextureHandle {
    let img = image::load_from_memory(bytes)
        .expect("icon png")
        .to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    ctx.load_texture(name, color, TextureOptions::LINEAR)
}
