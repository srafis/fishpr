//! Single instance and remote control over D-Bus. The running fishpr owns the
//! bus name `io.github.srafis.fishpr`, and `fishpr --toggle` asks it to start
//! or stop recording, like a tray click. That's the fallback where fishpr
//! can't register a global shortcut itself (GNOME 47 and older, most tiling
//! compositors): bind `fishpr --toggle` to a key in the desktop's settings.

use anyhow::{Result, bail};
use tokio::sync::mpsc::UnboundedSender;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

use crate::{APP_ID, Event};

const PATH: &str = "/io/github/srafis/fishpr";

struct Service {
    events: UnboundedSender<Event>,
}

#[zbus::interface(name = "io.github.srafis.fishpr")]
impl Service {
    fn toggle(&self) {
        let _ = self.events.send(Event::Toggle);
    }
}

#[zbus::proxy(interface = "io.github.srafis.fishpr", default_service = "io.github.srafis.fishpr", default_path = "/io/github/srafis/fishpr")]
trait Control {
    fn toggle(&self) -> zbus::Result<()>;
}

/// Claims the bus name. Fails if another fishpr already holds it. Keep the
/// connection alive for as long as fishpr runs.
pub async fn serve(events: UnboundedSender<Event>) -> Result<zbus::Connection> {
    let conn = zbus::connection::Builder::session()?.serve_at(PATH, Service { events })?.build().await?;
    // Not Builder::name: it ignores the reply, so a taken name looks like success.
    match conn.request_name_with_flags(APP_ID, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Ok(conn),
        Ok(_) | Err(zbus::Error::NameTaken) => bail!("fishpr is already running"),
        Err(e) => Err(anyhow::Error::from(e).context("claiming fishpr's D-Bus name")),
    }
}

/// Toggles recording in the running fishpr.
pub async fn toggle() -> Result<()> {
    let conn = zbus::Connection::session().await?;
    let proxy = ControlProxy::new(&conn).await?;
    proxy.toggle().await.map_err(|e| match e {
        zbus::Error::MethodError(name, ..) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" => {
            anyhow::anyhow!("fishpr isn't running")
        }
        e => anyhow::Error::from(e).context("asking fishpr to toggle recording"),
    })
}
