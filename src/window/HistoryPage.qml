import QtCore
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Dialogs
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import io.github.srafis.fishpr

Kirigami.ScrollablePage {
    id: page

    title: "History"

    History {
        id: history
        onEntriesChanged: page.sync()
    }

    // Picks up new transcriptions while the window is open.
    Timer {
        interval: 1000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: history.refresh()
    }

    ListModel {
        id: entries
    }

    Settings {
        id: settings

        category: "History"
        property bool confirmDelete: true
    }

    // Deletes the entry, asking first unless the user said not to.
    function remove(entryId) {
        if (settings.confirmDelete) {
            deleteDialog.entryId = entryId;
            dontAsk.checked = false;
            deleteDialog.open();
        } else {
            history.remove(entryId);
        }
    }

    Kirigami.Dialog {
        id: deleteDialog

        property string entryId

        function confirm() {
            if (dontAsk.checked) {
                settings.confirmDelete = false;
            }
            history.remove(entryId);
            close();
        }

        title: "Delete Transcription?"
        padding: Kirigami.Units.largeSpacing
        preferredWidth: Kirigami.Units.gridUnit * 22
        // Delete has focus, so Enter deletes; Esc cancels.
        onOpened: deleteButton.forceActiveFocus()
        onClosed: list.forceActiveFocus()

        footer: QQC2.DialogButtonBox {
            QQC2.Button {
                id: deleteButton

                text: "Delete"
                icon.name: "edit-delete"
                QQC2.DialogButtonBox.buttonRole: QQC2.DialogButtonBox.AcceptRole
            }
            QQC2.Button {
                text: "Cancel"
                icon.name: "dialog-cancel"
                QQC2.DialogButtonBox.buttonRole: QQC2.DialogButtonBox.RejectRole
            }
            onAccepted: deleteDialog.confirm()
            onRejected: deleteDialog.close()
        }

        // Buttons take Space but not Enter, so Enter confirms from anywhere in the dialog.
        Shortcut {
            sequences: ["Return", "Enter"]
            enabled: deleteDialog.opened
            onActivated: deleteDialog.confirm()
        }

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            QQC2.Label {
                Layout.fillWidth: true
                text: "Its text and recording will be deleted for good."
                wrapMode: Text.Wrap
            }
            QQC2.CheckBox {
                id: dontAsk

                text: "Don't ask again"
            }
        }
    }

    // Brings the list model in line with the history, touching only the rows
    // that changed, so the list keeps its place.
    function sync() {
        const fresh = JSON.parse(history.entries || "[]");
        // Rows added above keep the view where it is; at the top, show them.
        const atTop = list.atYBeginning;
        const ids = new Set(fresh.map(e => e.id));
        for (let i = entries.count - 1; i >= 0; i--) {
            if (!ids.has(entries.get(i).entryId)) {
                entries.remove(i);
            }
        }
        fresh.forEach((e, i) => {
            if (i >= entries.count || entries.get(i).entryId !== e.id) {
                entries.insert(i, {entryId: e.id, day: dayName(e.date), time: e.time, words: e.text});
            }
        });
        if (atTop) {
            list.positionViewAtBeginning();
        }
    }

    // "Today", "Yesterday", or the date, like "Monday, 5 October".
    function dayName(date) {
        const [y, m, d] = date.split("-").map(Number);
        const day = new Date(y, m - 1, d);
        const today = new Date();
        today.setHours(0, 0, 0, 0);
        const daysAgo = Math.round((today - day) / 86400000);
        if (daysAgo === 0) {
            return "Today";
        }
        if (daysAgo === 1) {
            return "Yesterday";
        }
        return day.toLocaleDateString(Qt.locale(), y === today.getFullYear() ? "dddd, d MMMM" : "dddd, d MMMM yyyy");
    }

    // The entry the arrow keys moved to, or that ⋯ was opened on.
    readonly property string current: list.currentIndex >= 0 ? entries.get(list.currentIndex).entryId : ""
    // Their shortcuts work on the current entry, and show in its ⋯ menu.
    readonly property bool canAct: current !== "" && !deleteDialog.opened

    QQC2.Action {
        id: saveAction

        text: "Save Recording As…"
        icon.name: "document-save-as"
        shortcut: "Ctrl+S"
        enabled: page.canAct
        onTriggered: {
            const folder = StandardPaths.writableLocation(StandardPaths.DocumentsLocation);
            saveDialog.entryId = page.current;
            saveDialog.currentFolder = folder;
            saveDialog.selectedFile = folder + "/fishpr " + page.current + ".wav";
            saveDialog.open();
        }
    }

    QQC2.Action {
        id: showAction

        text: "Show in Folder"
        icon.name: "document-open-folder"
        shortcut: "Return"
        enabled: page.canAct
        onTriggered: history.showInFolder(page.current)
    }

    QQC2.Action {
        id: deleteAction

        text: "Delete"
        icon.name: "edit-delete"
        shortcut: "Delete"
        enabled: page.canAct
        onTriggered: page.remove(page.current)
    }

    // One menu for every entry, so each shortcut is registered once.
    QQC2.Menu {
        id: menu

        QQC2.MenuItem {
            action: saveAction
        }
        QQC2.MenuItem {
            action: showAction
        }
        QQC2.MenuSeparator {}
        QQC2.MenuItem {
            action: deleteAction
        }
    }

    FileDialog {
        id: saveDialog

        property string entryId

        title: "Save Recording"
        fileMode: FileDialog.SaveFile
        nameFilters: ["WAV audio (*.wav)"]
        defaultSuffix: "wav"
        onAccepted: {
            if (!history.saveAudio(entryId, selectedFile)) {
                applicationWindow().showPassiveNotification("Couldn't save the recording");
            }
        }
    }

    ListView {
        id: list

        model: entries
        // Nothing to select: entries act through their buttons.
        currentIndex: -1

        // Keys for the entry the arrow keys moved to.
        Keys.onPressed: event => {
            if (currentIndex < 0) {
                return;
            }
            // The actions' shortcuts usually take Ctrl+S, Delete, and Return
            // before this sees them; either way, the action runs once.
            if (event.key === Qt.Key_Space) {
                history.play(page.current);
            } else if (event.matches(StandardKey.Copy)) {
                currentItem.copy();
            } else if (event.key === Qt.Key_S && event.modifiers === Qt.ControlModifier) {
                saveAction.trigger();
            } else if (event.key === Qt.Key_Delete) {
                deleteAction.trigger();
            } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                showAction.trigger();
            } else {
                return;
            }
            event.accepted = true;
        }

        section.property: "day"
        section.delegate: Kirigami.ListSectionHeader {
            required property string section

            width: ListView.view.width
            text: section
        }

        delegate: QQC2.ItemDelegate {
            id: entry

            required property string entryId
            required property string time
            required property string words
            required property int index

            // How the last copy went, which the copy button shows for a moment.
            property string copied: ""

            function copy() {
                copied = history.copy(entryId) ? "yes" : "failed";
                copiedFor.restart();
                pop.restart();
            }

            Timer {
                id: copiedFor

                interval: 1500
                onTriggered: entry.copied = ""
            }

            // The copy button pops as its icon changes.
            NumberAnimation {
                id: pop

                target: copyButton
                property: "scale"
                from: 0.6
                to: 1
                duration: Kirigami.Units.longDuration
                easing.type: Easing.OutBack
            }

            width: ListView.view.width
            hoverEnabled: true
            // A row is a button, and would take Space as a press of itself.
            Keys.onSpacePressed: history.play(entryId)
            highlighted: false
            down: false

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                QQC2.Label {
                    Layout.alignment: Qt.AlignTop
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 3
                    text: entry.time
                    color: Kirigami.Theme.disabledTextColor
                }

                QQC2.Label {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignTop
                    text: entry.words
                    wrapMode: Text.Wrap
                    maximumLineCount: 4
                    elide: Text.ElideRight
                    textFormat: Text.PlainText
                }

                RowLayout {
                    Layout.alignment: Qt.AlignTop
                    spacing: 0

                    QQC2.ToolButton {
                        id: copyButton

                        icon.name: ({yes: "checkmark", failed: "dialog-error"})[entry.copied] ?? "edit-copy"
                        text: ({yes: "Copied", failed: "Couldn't copy"})[entry.copied] ?? "Copy"
                        display: QQC2.AbstractButton.IconOnly
                        // Clicking leaves focus on the row, so its keys keep working.
                        focusPolicy: Qt.TabFocus
                        QQC2.ToolTip.text: entry.copied ? text : text + " (Ctrl+C)"
                        QQC2.ToolTip.visible: hovered
                        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                        onClicked: entry.copy()
                    }

                    QQC2.ToolButton {
                        readonly property bool playing: history.playing === entry.entryId

                        icon.name: playing ? "media-playback-stop" : "media-playback-start"
                        text: playing ? "Stop" : "Play Recording"
                        display: QQC2.AbstractButton.IconOnly
                        // Clicking leaves focus on the row, so its keys keep working.
                        focusPolicy: Qt.TabFocus
                        QQC2.ToolTip.text: text + " (Space)"
                        QQC2.ToolTip.visible: hovered
                        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                        onClicked: history.play(entry.entryId)
                    }

                    QQC2.ToolButton {
                        id: more

                        icon.name: "overflow-menu"
                        text: "More"
                        display: QQC2.AbstractButton.IconOnly
                        // Clicking leaves focus on the row, so its keys keep working.
                        focusPolicy: Qt.TabFocus
                        down: menu.visible && page.current === entry.entryId
                        QQC2.ToolTip.text: text
                        QQC2.ToolTip.visible: hovered && !menu.visible
                        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                        onClicked: {
                            list.currentIndex = entry.index;
                            menu.popup(more, 0, more.height);
                        }
                    }
                }
            }
        }

        Kirigami.PlaceholderMessage {
            anchors.centerIn: parent
            width: parent.width - Kirigami.Units.gridUnit * 4
            visible: list.count === 0
            icon.name: "view-history"
            text: "No transcriptions yet"
            explanation: "Hold Ctrl+Space and speak. What you say shows up here, with its recording."
        }
    }
}
