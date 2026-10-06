//! The fishpr window: a Kirigami app that lists the history. It runs as its
//! own process, `fishpr --window`, which the tray menu starts, so the dictation
//! itself never waits on Qt. A second one asks the first to come forward, and
//! exits.

mod app;
mod model;

use std::sync::OnceLock;

use anyhow::Result;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

const BUS_NAME: &str = "io.github.srafis.fishpr.Window";
const PATH: &str = "/io/github/srafis/fishpr/Window";

/// Runs async work for the window: D-Bus, which zbus needs a runtime for.
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().expect("tokio runtime"))
}

/// Starts the window from the running fishpr, without waiting for it.
pub fn open() {
    // After an upgrade, the running binary's path reads "/usr/bin/fishpr (deleted)";
    // the new one is at the same path.
    let exe = std::env::current_exe().map(|exe| {
        let path = exe.to_string_lossy();
        path.strip_suffix(" (deleted)").map(Into::into).unwrap_or(exe)
    });
    match exe.and_then(|exe| std::process::Command::new(exe).arg("--window").spawn()) {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => crate::desktop::notify("Couldn't open fishpr", &format!("{e:#}")),
    }
}

struct Service;

#[zbus::interface(name = "io.github.srafis.fishpr.Window")]
impl Service {
    fn activate(&self) {
        app::ffi::activate_window();
    }
}

#[zbus::proxy(interface = "io.github.srafis.fishpr.Window", default_service = "io.github.srafis.fishpr.Window", default_path = "/io/github/srafis/fishpr/Window")]
trait Window {
    fn activate(&self) -> zbus::Result<()>;
}

/// Shows the window until it's closed, or brings forward the one already open.
pub fn run() -> Result<()> {
    let _bus = match runtime().block_on(claim()) {
        Ok(Some(conn)) => conn,
        Ok(None) => {
            return runtime().block_on(async {
                let conn = zbus::Connection::session().await?;
                WindowProxy::new(&conn).await?.activate().await?;
                Ok(())
            });
        }
        Err(e) => return Err(e),
    };
    cxx_qt::init_crate!(cxx_qt_lib);
    cxx_qt::init_qml_module!("io.github.srafis.fishpr");
    match app::ffi::run_window() {
        0 => Ok(()),
        code => anyhow::bail!("the window exited with {code}"),
    }
}

/// Serves the window's D-Bus name, or None if another window has it.
async fn claim() -> Result<Option<zbus::Connection>> {
    let conn = zbus::connection::Builder::session()?.serve_at(PATH, Service)?.build().await?;
    match conn.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Ok(Some(conn)),
        Ok(_) | Err(zbus::Error::NameTaken) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
