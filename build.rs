use cxx_qt_build::{CxxQtBuilder, QResource, QResources, QmlModule};

fn main() {
    // The QML module only registers `History`. The QML files are plain
    // resources, compiled when the window opens: qmlcachegen's ahead-of-time
    // code would tie the binary to the exact Qt version it was built with.
    let builder = CxxQtBuilder::new_qml_module(QmlModule::new("io.github.srafis.fishpr"))
        .qt_module("Gui")
        .qt_module("Widgets")
        .qt_module("Quick")
        .qt_module("QuickControls2")
        .qrc_resources(QResources::new().resource(QResource::new().prefix("/fishpr").files([
            "assets/icon.png",
            "src/window/Main.qml",
            "src/window/HistoryPage.qml",
        ])))
        .files(["src/window/app.rs", "src/window/model.rs"])
        .cpp_file("src/window/app.cpp");
    // Qt's headers set off this GCC 16 warning by the hundred.
    let builder = unsafe { builder.cc_builder(|cc| _ = cc.flag_if_supported("-Wno-sfinae-incomplete")) };
    builder.build();
}
