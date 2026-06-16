//! Desktop notifications via the freedesktop D-Bus interface.

use std::collections::HashMap;

use zbus::zvariant::Value;

/// Notification urgency levels per the freedesktop spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    /// Map a free-text urgency to the spec's byte hint, defaulting to Normal.
    pub fn from_opt(s: Option<&str>) -> Self {
        match s.map(str::to_ascii_lowercase).as_deref() {
            Some("low") => Urgency::Low,
            Some("critical") => Urgency::Critical,
            _ => Urgency::Normal,
        }
    }

    fn hint(self) -> u8 {
        match self {
            Urgency::Low => 0,
            Urgency::Normal => 1,
            Urgency::Critical => 2,
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

/// Send a desktop notification, returning the server-assigned id.
pub async fn send(
    conn: &zbus::Connection,
    title: &str,
    body: &str,
    urgency: Urgency,
    icon: Option<&str>,
) -> zbus::Result<u32> {
    let proxy = NotificationsProxy::new(conn).await?;
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("urgency", Value::U8(urgency.hint()));
    proxy
        .notify(
            "Roci",
            0,
            icon.unwrap_or_default(),
            title,
            body,
            &[],
            hints,
            -1,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_mapping() {
        assert_eq!(Urgency::from_opt(Some("low")), Urgency::Low);
        assert_eq!(Urgency::from_opt(Some("LOW")), Urgency::Low);
        assert_eq!(Urgency::from_opt(Some("critical")), Urgency::Critical);
        assert_eq!(Urgency::from_opt(Some("normal")), Urgency::Normal);
        assert_eq!(Urgency::from_opt(None), Urgency::Normal);
        assert_eq!(Urgency::from_opt(Some("bogus")), Urgency::Normal);
    }

    #[test]
    fn urgency_hints() {
        assert_eq!(Urgency::Low.hint(), 0);
        assert_eq!(Urgency::Normal.hint(), 1);
        assert_eq!(Urgency::Critical.hint(), 2);
    }
}
