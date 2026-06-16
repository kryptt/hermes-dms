import QtQuick
import QtQuick.Layouts
import qs.Common
import qs.Widgets
import "markdown2html.js" as Md

Item {
    id: chatRoot

    signal escapePressed()

    // Index of the assistant message currently being streamed (-1 = none).
    property int streamIndex: -1

    // --- Header: title, connection status, new-conversation ---
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
                        messageModel.clear();
                        chatRoot.streamIndex = -1;
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

    // --- Messages (fills the space between header and input) ---
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
                    model: ListModel {
                        id: messageModel
                    }

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

    function sendCurrentMessage() {
        var text = inputField.text.trim();
        if (!text || HermesService.busy)
            return;
        inputField.text = "";
        HermesService.sendMessage(text);
    }

    Component.onCompleted: inputField.forceActiveFocus()

    Connections {
        target: HermesService

        function onUserMessage(text) {
            messageModel.append({ msgRole: "user", msgContent: text });
        }
        function onAssistantStarted() {
            messageModel.append({ msgRole: "assistant", msgContent: "" });
            chatRoot.streamIndex = messageModel.count - 1;
        }
        function onAssistantDelta(chunk) {
            if (chatRoot.streamIndex < 0)
                return;
            var prev = messageModel.get(chatRoot.streamIndex).msgContent || "";
            messageModel.setProperty(chatRoot.streamIndex, "msgContent", prev + chunk);
        }
        function onAssistantFinished(fullContent) {
            if (chatRoot.streamIndex < 0) {
                messageModel.append({ msgRole: "assistant", msgContent: fullContent });
            } else {
                var cur = messageModel.get(chatRoot.streamIndex).msgContent || "";
                // Prefer the authoritative final text when nothing was streamed.
                if (!cur && fullContent)
                    messageModel.setProperty(chatRoot.streamIndex, "msgContent", fullContent);
            }
            chatRoot.streamIndex = -1;
        }
        function onErrorOccurred(message) {
            chatRoot.streamIndex = -1;
            messageModel.append({ msgRole: "error", msgContent: message });
        }
        function onSessionRefreshed() {
            messageModel.append({ msgRole: "assistant", msgContent: "_Session refreshed._" });
        }
    }
}
