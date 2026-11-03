//! Lightweight Windows tray / toast notifications.

/// Show a short notification when `enabled` (respects `misc.tray_notifications`).
pub fn show(enabled: bool, title: &str, body: &str) {
    if !enabled {
        return;
    }
    #[cfg(windows)]
    {
        win::show(title, body);
    }
    #[cfg(not(windows))]
    {
        let _ = (title, body);
        tracing::info!(%title, %body, "notify (no tray on this platform)");
    }
}

#[cfg(windows)]
mod win {
    use winrt_notification::{Duration, Sound, Toast};

    pub fn show(title: &str, body: &str) {
        // POWERSHELL_APP_ID works without a packaged AppUserModelID.
        if let Err(e) = Toast::new(Toast::POWERSHELL_APP_ID)
            .title(title)
            .text1(body)
            .sound(Some(Sound::Default))
            .duration(Duration::Short)
            .show()
        {
            tracing::debug!("toast failed: {e} — falling back to tracing");
            tracing::info!(%title, %body, "tray notification");
        }
    }
}

// Only the `enabled == false` gate is tested: with the option off `show` must return before
// reaching the toast API. The enabled path would pop a real Windows toast, so it is skipped.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_notifications_return_immediately() {
        show(false, "title", "body");
        show(false, "", "");
        show(false, "żółć ✓", "treść\nwielolinijkowa");
    }
}
