import QtQuick
import QtQuick.Layouts
import qs.Common
import qs.Widgets
import "markdown2html.js" as Md

Item {
    id: chatRoot

    signal escapePressed()

    property bool modelMenuOpen: false
    property bool sessionMenuOpen: false

    function sessionLabel(s) {
        if (s.preview && s.preview.trim())
            return s.preview;
        var t = s.title || s.id;
        return t.replace(/^\[Desktop\]\s*/, "");
    }

    // --- Header: title, status, sessions, new-conversation ---
    Item {
        id: header
        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        height: 36

        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 10
            anchors.rightMargin: 8
            spacing: 8

            DankIcon {
                name: "smart_toy"
                color: Theme.primary
                size: 18
            }
            StyledText {
                text: "Roci"
                color: Theme.surfaceText
                font.pixelSize: Theme.fontSizeMedium
                font.weight: Font.Medium
            }
            // Connection status dot.
            Rectangle {
                width: 8
                height: 8
                radius: 4
                color: HermesService.connected ? "#4CAF50" : (HermesService.daemonReady ? "#FF9800" : "#EF4444")
                Layout.alignment: Qt.AlignVCenter
            }
            StyledText {
                text: HermesService.statusText
                color: Theme.surfaceVariantText
                font.pixelSize: Theme.fontSizeSmall
                Layout.fillWidth: true
                elide: Text.ElideRight
            }
            // Session switcher.
            Rectangle {
                width: 26
                height: 26
                radius: 13
                color: sessionsArea.containsMouse || chatRoot.sessionMenuOpen ? Theme.withAlpha(Theme.surfaceVariant, 0.3) : "transparent"
                DankIcon {
                    anchors.centerIn: parent
                    name: "forum"
                    color: Theme.surfaceVariantText
                    size: 16
                }
                MouseArea {
                    id: sessionsArea
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: {
                        if (!chatRoot.sessionMenuOpen)
                            HermesService.listSessions();
                        chatRoot.sessionMenuOpen = !chatRoot.sessionMenuOpen;
                        chatRoot.modelMenuOpen = false;
                    }
                }
            }
            // New conversation.
            Rectangle {
                width: 26
                height: 26
                radius: 13
                color: newChatArea.containsMouse ? Theme.withAlpha(Theme.surfaceVariant, 0.3) : "transparent"
                DankIcon {
                    anchors.centerIn: parent
                    name: "add"
                    color: Theme.surfaceVariantText
                    size: 16
                }
                MouseArea {
                    id: newChatArea
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: {
                        chatRoot.sessionMenuOpen = false;
                        HermesService.newConversation();
                    }
                }
            }
        }
    }

    // --- Input card (anchored to bottom) ---
    Rectangle {
        id: inputCard
        anchors.bottom: parent.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        height: inputCol.height
        radius: 20
        color: Theme.surfaceContainer
        border.width: 1
        border.color: Theme.outlineVariant
        z: 10

        ColumnLayout {
            id: inputCol
            width: parent.width
            spacing: 0

            Item {
                Layout.fillWidth: true
                Layout.preferredHeight: Math.max(44, inputField.contentHeight + 24)

                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.IBeamCursor
                    onClicked: inputField.forceActiveFocus()
                }

                TextEdit {
                    id: inputField
                    anchors.fill: parent
                    anchors.leftMargin: 18
                    anchors.rightMargin: 18
                    anchors.topMargin: 12
                    anchors.bottomMargin: 12
                    color: Theme.surfaceText
                    font.pixelSize: 14
                    wrapMode: TextEdit.Wrap
                    clip: true

                    Text {
                        visible: !inputField.text && !inputField.activeFocus
                        text: "Message Roci…"
                        color: Theme.surfaceVariantText
                        font.pixelSize: 14
                        anchors.verticalCenter: parent.verticalCenter
                    }

                    Keys.onReturnPressed: function (event) {
                        if (event.modifiers & Qt.ShiftModifier)
                            event.accepted = false;
                        else {
                            event.accepted = true;
                            sendCurrentMessage();
                        }
                    }
                    Keys.onEscapePressed: function (event) {
                        event.accepted = true;
                        chatRoot.escapePressed();
                    }
                }
            }

            Rectangle {
                Layout.fillWidth: true
                Layout.leftMargin: 14
                Layout.rightMargin: 14
                height: 1
                color: Theme.withAlpha(Theme.outlineVariant, 0.3)
            }

            Item {
                Layout.fillWidth: true
                Layout.preferredHeight: 40

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    anchors.rightMargin: 8
                    spacing: 4

                    // Model picker pill (opens modelMenu above the input).
                    Rectangle {
                        Layout.preferredHeight: 26
                        Layout.preferredWidth: modelRow.implicitWidth + 16
                        radius: 13
                        color: modelPillArea.containsMouse || chatRoot.modelMenuOpen ? Theme.withAlpha(Theme.surfaceVariant, 0.3) : "transparent"
                        Row {
                            id: modelRow
                            anchors.centerIn: parent
                            spacing: 4
                            StyledText {
                                text: HermesService.activeModelLabel()
                                font.pixelSize: 11
                                color: Theme.surfaceVariantText
                                anchors.verticalCenter: parent.verticalCenter
                            }
                            DankIcon {
                                name: chatRoot.modelMenuOpen ? "expand_more" : "expand_less"
                                size: 14
                                color: Theme.surfaceVariantText
                                anchors.verticalCenter: parent.verticalCenter
                            }
                        }
                        MouseArea {
                            id: modelPillArea
                            anchors.fill: parent
                            hoverEnabled: true
                            onClicked: {
                                if (!chatRoot.modelMenuOpen)
                                    HermesService.requestModels();
                                chatRoot.modelMenuOpen = !chatRoot.modelMenuOpen;
                                chatRoot.sessionMenuOpen = false;
                            }
                        }
                    }

                    Item {
                        Layout.fillWidth: true
                    }

                    // Busy indicator + cancel
                    Row {
                        spacing: 6
                        anchors.verticalCenter: parent.verticalCenter
                        visible: HermesService.busy

                        Rectangle {
                            width: 6
                            height: 6
                            radius: 3
                            anchors.verticalCenter: parent.verticalCenter
                            color: Theme.primary
                            SequentialAnimation on opacity {
                                running: HermesService.busy
                                loops: Animation.Infinite
                                NumberAnimation {
                                    to: 0.2
                                    duration: 600
                                    easing.type: Easing.InOutSine
                                }
                                NumberAnimation {
                                    to: 1.0
                                    duration: 600
                                    easing.type: Easing.InOutSine
                                }
                            }
                        }
                        Rectangle {
                            width: 22
                            height: 22
                            radius: 11
                            anchors.verticalCenter: parent.verticalCenter
                            color: cancelArea.containsMouse ? Theme.withAlpha(Theme.error || "#EF4444", 0.15) : "transparent"
                            DankIcon {
                                anchors.centerIn: parent
                                name: "close"
                                color: Theme.surfaceVariantText
                                size: 14
                            }
                            MouseArea {
                                id: cancelArea
                                anchors.fill: parent
                                hoverEnabled: true
                                onClicked: HermesService.cancel()
                            }
                        }
                    }

                    // Send button
                    Rectangle {
                        visible: !HermesService.busy
                        width: 32
                        height: 32
                        radius: 16
                        property bool canSend: inputField.text.trim().length > 0
                        color: canSend ? Theme.primary : Theme.withAlpha(Theme.surfaceVariant, 0.2)
                        DankIcon {
                            anchors.centerIn: parent
                            name: "arrow_upward"
                            color: parent.canSend ? Theme.primaryText : Theme.surfaceVariantText
                            size: 18
                        }
                        MouseArea {
                            anchors.fill: parent
                            onClicked: if (parent.canSend)
                                sendCurrentMessage()
                        }
                    }
                }
            }
        }
    }

    // --- Messages (bound to the persistent singleton model) ---
    Flickable {
        id: messageFlick
        anchors.top: header.bottom
        anchors.bottom: inputCard.top
        anchors.bottomMargin: 12
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.leftMargin: 8
        anchors.rightMargin: 8
        clip: true
        contentHeight: messageColumn.height
        contentWidth: width

        function scrollToEnd() {
            if (contentHeight > height)
                contentY = contentHeight - height;
        }

        Column {
            id: messageColumn
            width: parent.width
            spacing: 6

            Item {
                width: 1
                height: Math.max(0, messageFlick.height - messagesContent.height)
            }

            Column {
                id: messagesContent
                width: parent.width
                spacing: 6

                Repeater {
                    model: HermesService.messages

                    Loader {
                        width: messagesContent.width
                        sourceComponent: {
                            if (model.msgRole === "user")
                                return userComp;
                            if (model.msgRole === "error")
                                return errorComp;
                            return assistantComp;
                        }
                        property string content: model.msgContent || ""
                    }
                }
            }
        }

        onContentHeightChanged: Qt.callLater(scrollToEnd)
    }

    // --- Bubbles ---
    Component {
        id: userComp
        Item {
            height: uRect.height
            Rectangle {
                id: uRect
                anchors.right: parent.right
                width: Math.min(parent.width * 0.8, uTxt.implicitWidth + 28)
                height: uTxt.implicitHeight + 20
                radius: 16
                color: Theme.primary
                Text {
                    id: uTxt
                    anchors.fill: parent
                    anchors.margins: 10
                    anchors.leftMargin: 14
                    anchors.rightMargin: 14
                    text: content
                    wrapMode: Text.Wrap
                    color: Theme.primaryText
                    font.pixelSize: 13
                    lineHeight: 1.3
                }
            }
        }
    }

    Component {
        id: assistantComp
        Item {
            height: aRect.height
            Rectangle {
                id: aRect
                anchors.left: parent.left
                width: Math.min(parent.width * 0.85, aTxt.implicitWidth + 28)
                height: aTxt.implicitHeight + 20
                radius: 16
                color: Theme.surfaceContainer
                Text {
                    id: aTxt
                    anchors.fill: parent
                    anchors.margins: 10
                    anchors.leftMargin: 14
                    anchors.rightMargin: 14
                    text: content ? Md.markdownToHtml(content) : "…"
                    textFormat: Text.RichText
                    wrapMode: Text.Wrap
                    color: Theme.surfaceText
                    font.pixelSize: 13
                    lineHeight: 1.4
                    onLinkActivated: function (link) {
                        Quickshell.execDetached(["xdg-open", link]);
                    }
                }
            }
        }
    }

    Component {
        id: errorComp
        Item {
            height: eRect.height
            Rectangle {
                id: eRect
                anchors.left: parent.left
                width: Math.min(parent.width * 0.85, eTxt.implicitWidth + 28)
                height: eTxt.implicitHeight + 20
                radius: 16
                color: Theme.withAlpha(Theme.error || "#EF4444", 0.15)
                Text {
                    id: eTxt
                    anchors.fill: parent
                    anchors.margins: 10
                    anchors.leftMargin: 14
                    anchors.rightMargin: 14
                    text: content
                    wrapMode: Text.Wrap
                    color: Theme.error || "#EF4444"
                    font.pixelSize: 13
                }
            }
        }
    }

    // Session switcher dropdown — opens below the header. Highlights the active
    // session; picking one resumes it and replays its history.
    Rectangle {
        id: sessionMenu
        visible: chatRoot.sessionMenuOpen
        anchors.top: header.bottom
        anchors.right: header.right
        anchors.rightMargin: 8
        width: 320
        height: Math.min(360, Math.max(40, sessionList.contentHeight + 8))
        radius: 12
        color: Theme.surfaceContainerHigh
        border.width: 1
        border.color: Theme.outlineVariant
        z: 30

        StyledText {
            visible: sessionList.count === 0
            anchors.centerIn: parent
            text: "No sessions yet"
            color: Theme.surfaceVariantText
            font.pixelSize: 12
        }

        ListView {
            id: sessionList
            anchors.fill: parent
            anchors.margins: 4
            clip: true
            model: HermesService.sessions
            delegate: Rectangle {
                width: ListView.view.width
                height: 44
                radius: 8
                color: sessRowArea.containsMouse ? Theme.withAlpha(Theme.surfaceVariant, 0.3) : "transparent"
                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 10
                    anchors.rightMargin: 10
                    spacing: 8
                    Rectangle {
                        width: 6
                        height: 6
                        radius: 3
                        color: Theme.primary
                        opacity: modelData.id === HermesService.sessionId ? 1 : 0
                        Layout.alignment: Qt.AlignVCenter
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 0
                        StyledText {
                            text: chatRoot.sessionLabel(modelData)
                            font.pixelSize: 12
                            color: Theme.surfaceText
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                        StyledText {
                            text: (modelData.model || "") + (modelData.message_count ? "  ·  " + modelData.message_count + " msgs" : "")
                            font.pixelSize: 10
                            color: Theme.surfaceVariantText
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                    }
                }
                MouseArea {
                    id: sessRowArea
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: {
                        HermesService.resumeSession(modelData.id);
                        chatRoot.sessionMenuOpen = false;
                    }
                }
            }
        }
    }

    // Model picker dropdown — opens upward from the input pill. Green dot =
    // loaded in ollama-router; check = active. Picking one starts a fresh
    // session on that model (Hermes binds the model at session creation).
    Rectangle {
        id: modelMenu
        visible: chatRoot.modelMenuOpen
        anchors.bottom: inputCard.top
        anchors.bottomMargin: 6
        anchors.left: inputCard.left
        anchors.leftMargin: 8
        width: 260
        height: Math.min(300, modelList.contentHeight + 8)
        radius: 12
        color: Theme.surfaceContainerHigh
        border.width: 1
        border.color: Theme.outlineVariant
        z: 30

        ListView {
            id: modelList
            anchors.fill: parent
            anchors.margins: 4
            clip: true
            model: HermesService.models
            delegate: Rectangle {
                width: ListView.view.width
                height: 32
                radius: 8
                color: rowArea.containsMouse ? Theme.withAlpha(Theme.surfaceVariant, 0.3) : "transparent"
                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 10
                    anchors.rightMargin: 10
                    spacing: 8
                    Rectangle {
                        width: 7
                        height: 7
                        radius: 3.5
                        color: modelData.loaded ? "#4CAF50" : Theme.withAlpha(Theme.surfaceVariantText, 0.4)
                        Layout.alignment: Qt.AlignVCenter
                    }
                    StyledText {
                        text: modelData.id
                        font.pixelSize: 12
                        color: Theme.surfaceText
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    DankIcon {
                        visible: modelData.id === HermesService.selectedModel
                        name: "check"
                        size: 14
                        color: Theme.primary
                    }
                }
                MouseArea {
                    id: rowArea
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: {
                        HermesService.setModel(modelData.id);
                        chatRoot.modelMenuOpen = false;
                    }
                }
            }
        }
    }

    function sendCurrentMessage() {
        var text = inputField.text.trim();
        if (!text || HermesService.busy)
            return;
        inputField.text = "";
        HermesService.sendMessage(text);
    }

    // The popout force-focuses its own container via Qt.callLater on open,
    // which would steal focus from the input. Re-grab it a tick later, and
    // snap the (restored) transcript to the bottom.
    Component.onCompleted: focusTimer.start()
    Timer {
        id: focusTimer
        interval: 80
        repeat: false
        onTriggered: {
            inputField.forceActiveFocus();
            messageFlick.scrollToEnd();
        }
    }
}
