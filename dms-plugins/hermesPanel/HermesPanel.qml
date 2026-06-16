import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Wayland
import Quickshell.Io
import qs.Common
import qs.Services
import qs.Widgets
import qs.Modules.Plugins

PluginComponent {
    id: root

    layerNamespacePlugin: "hermesPanel"

    // Toggle the panel from a keyboard shortcut: `dms ipc call hermesPanel toggle`.
    IpcHandler {
        function toggle(): string {
            hermesPanel.toggle();
            return hermesPanel.isVisible ? "opened" : "closed";
        }
        target: "hermesPanel"
    }

    PanelWindow {
        id: hermesPanel

        property bool isVisible: false

        function show() {
            // Open on the monitor with the focused window/workspace, not always
            // screen 0 (CompositorService is niri-aware via NiriService.currentOutput).
            screen = CompositorService.getFocusedScreen();
            visible = true;
            isVisible = true;
            HermesService.popoutVisible = true;
            animScale = 1.0;
            animOpacity = 1.0;
        }
        function hide() {
            isVisible = false;
            HermesService.popoutVisible = false;
            animScale = 0.92;
            animOpacity = 0.0;
        }
        function toggle() {
            if (isVisible)
                hide();
            else
                show();
        }

        property real animScale: 0.92
        property real animOpacity: 0.0

        visible: isVisible || hideAnim.running || scaleAnim.running
        screen: CompositorService.getFocusedScreen()
        color: "transparent"

        anchors.bottom: true

        WlrLayershell.layer: WlrLayershell.Top
        WlrLayershell.namespace: "dms:hermes"
        WlrLayershell.exclusiveZone: 0
        WlrLayershell.keyboardFocus: isVisible ? WlrKeyboardFocus.OnDemand : WlrKeyboardFocus.None
        WlrLayershell.margins.bottom: 44

        implicitWidth: 660
        implicitHeight: 740

        Item {
            id: animContainer
            anchors.fill: parent
            anchors.margins: 10
            scale: hermesPanel.animScale
            opacity: hermesPanel.animOpacity
            transformOrigin: Item.Bottom

            // Translucent backdrop so the chat stays legible over whatever's
            // behind it (e.g. a terminal). Uses the DMS popout convention
            // (surfaceContainer at the user's popupTransparency setting).
            Rectangle {
                anchors.fill: parent
                radius: 20
                color: Theme.panelBackground()
                border.width: 1
                border.color: Theme.outlineVariant
            }

            HermesPanelChat {
                id: hermesChat
                anchors.fill: parent
                onEscapePressed: hermesPanel.hide()
            }
        }

        Behavior on animScale {
            NumberAnimation {
                id: scaleAnim
                duration: 250
                easing.type: Easing.OutCubic
            }
        }

        Behavior on animOpacity {
            NumberAnimation {
                id: hideAnim
                duration: 200
                easing.type: Easing.OutCubic
                onRunningChanged: {
                    if (!running && !hermesPanel.isVisible)
                        hermesPanel.visible = false;
                }
            }
        }
    }

    horizontalBarPill: Component {
        Row {
            spacing: Theme.spacingXS
            DankIcon {
                name: HermesService.busy ? "hourglass_top" : "smart_toy"
                color: HermesService.busy ? "#FF9800" : (HermesService.connected ? Theme.primary : Theme.surfaceVariantText)
                size: root.iconSize
                anchors.verticalCenter: parent.verticalCenter
            }
            StyledText {
                anchors.verticalCenter: parent.verticalCenter
                text: HermesService.busy ? "Roci…" : "Roci"
                color: Theme.surfaceText
                font.pixelSize: Theme.fontSizeSmall
            }
        }
    }

    verticalBarPill: Component {
        Column {
            spacing: 2
            DankIcon {
                name: HermesService.busy ? "hourglass_top" : "smart_toy"
                color: HermesService.busy ? "#FF9800" : (HermesService.connected ? Theme.primary : Theme.surfaceVariantText)
                size: root.iconSize
                anchors.horizontalCenter: parent.horizontalCenter
            }
        }
    }

    pillClickAction: function () {
        hermesPanel.toggle();
    }
}
