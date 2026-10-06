#include "fishpr/src/window/app.h"

#include <QApplication>
#include <QIcon>
#include <QQmlApplicationEngine>
#include <QQuickStyle>
#include <QUrl>
#include <QWindow>

// A QApplication rather than a QGuiApplication: Plasma's style for Qt Quick
// controls (org.kde.desktop) draws them with the desktop's widget style.
int run_window()
{
    static int argc = 1;
    static char name[] = "fishpr";
    static char *argv[] = {name, nullptr};
    QApplication app(argc, argv);
    // Settings go in ~/.config/fishpr/fishpr.conf.
    QApplication::setOrganizationName(QStringLiteral("fishpr"));
    QApplication::setApplicationName(QStringLiteral("fishpr"));
    QApplication::setApplicationDisplayName(QStringLiteral("fishpr"));
    // Ties the window to fishpr's desktop entry, for its taskbar icon and name.
    QApplication::setDesktopFileName(QStringLiteral("io.github.srafis.fishpr"));
    QApplication::setWindowIcon(QIcon(QStringLiteral(":/fishpr/assets/icon.png")));
    if (qEnvironmentVariableIsEmpty("QT_QUICK_CONTROLS_STYLE")) {
        QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));
    }

    QQmlApplicationEngine engine;
    engine.load(QUrl(QStringLiteral("qrc:/fishpr/src/window/Main.qml")));
    if (engine.rootObjects().isEmpty()) {
        return 1;
    }
    return app.exec();
}

void activate_window()
{
    QMetaObject::invokeMethod(
        qApp,
        [] {
            for (QWindow *window : QGuiApplication::topLevelWindows()) {
                if (window->isVisible()) {
                    window->raise();
                    window->requestActivate();
                }
            }
        },
        Qt::QueuedConnection);
}
