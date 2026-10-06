import QtQuick
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami

Kirigami.ApplicationWindow {
    id: root

    title: "fishpr"
    width: Kirigami.Units.gridUnit * 48
    height: Kirigami.Units.gridUnit * 34
    minimumWidth: Kirigami.Units.gridUnit * 26
    minimumHeight: Kirigami.Units.gridUnit * 20

    // A sidebar like System Settings has, with one page for now.
    globalDrawer: Kirigami.GlobalDrawer {
        isMenu: false
        modal: !root.wideScreen
        handleVisible: modal
        width: Kirigami.Units.gridUnit * 12

        header: Kirigami.AbstractApplicationHeader {
            contentItem: Row {
                spacing: Kirigami.Units.smallSpacing
                leftPadding: Kirigami.Units.largeSpacing

                Image {
                    anchors.verticalCenter: parent.verticalCenter
                    source: "qrc:/fishpr/assets/icon.png"
                    width: Kirigami.Units.iconSizes.medium
                    height: width
                    sourceSize: Qt.size(width * 2, height * 2)
                    fillMode: Image.PreserveAspectFit
                }
                Kirigami.Heading {
                    anchors.verticalCenter: parent.verticalCenter
                    level: 2
                    text: "fishpr"
                }
            }
        }

        // One page is always the current one: clicking it again keeps it.
        QQC2.ActionGroup {
            id: pages
            exclusive: true
        }

        actions: [
            Kirigami.Action {
                text: "History"
                icon.name: "view-history"
                checkable: true
                checked: true
                QQC2.ActionGroup.group: pages
            }
        ]
    }

    // Ctrl+W, like other windows.
    Shortcut {
        sequences: [StandardKey.Close]
        onActivated: root.close()
    }

    pageStack.initialPage: HistoryPage {}
    pageStack.globalToolBar.style: Kirigami.ApplicationHeaderStyle.ToolBar
}
